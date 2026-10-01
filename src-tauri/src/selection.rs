//! FFI bindings and service wrapper around `SelectionHook.dll` — the C++ selection engine
//! ported from selection-hook (see `native/`).
//!
//! # Why a DLL
//!
//! Text selection across arbitrary Windows apps needs the low-level mouse hook, UIAutomation,
//! MSAA and a clipboard fallback. That logic already exists, is battle-tested, and is hard to
//! port: rewriting it in Rust would change behaviour and risk regressions. Instead the C++
//! sources are compiled unchanged into a DLL with a thin C ABI (`native/shim.cpp`), and this
//! module is the Rust half of that boundary.
//!
//! # Threading contract (important)
//!
//! The DLL runs its own threads — a low-level hook thread, a dispatch thread that classifies
//! gestures, and an MTA worker that performs retrieval. Callbacks therefore arrive on
//! *non-main* threads:
//!
//! * Callbacks must not block. All the work here is copying the payload and emitting a Tauri
//!   event; the heavy lifting stays in the DLL.
//! * Strings handed to a callback are only valid for that call, so they are copied into owned
//!   `String`s before anything else happens.
//! * `HOOK` is an `AtomicPtr` because it is read by callbacks and written by the main thread.
//!
//! # Lifetime
//!
//! `ensure_created` allocates the shim lazily; `cleanup` releases it and is safe to call more
//! than once (it swaps the pointer). The hook is only *started* on demand, so a disabled
//! feature costs nothing.

use std::ffi::{c_char, c_void, CStr};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// C ABI
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct SPoint {
    pub x: i32,
    pub y: i32,
}
#[repr(C)]
pub struct SSelection {
    pub text: *const c_char,
    pub program_name: *const c_char,
    pub start_top: SPoint,
    pub start_bottom: SPoint,
    pub end_top: SPoint,
    pub end_bottom: SPoint,
    pub mouse_pos_start: SPoint,
    pub mouse_pos_end: SPoint,
    pub method: i32,
    pub pos_level: i32,
}

#[repr(C)]
pub struct SMouseEvent {
    pub action: i32,
    pub x: i32,
    pub y: i32,
    pub button: i32,
    pub flag: i32,
}

#[repr(C)]
pub struct SKeyEvent {
    pub action: i32,
    pub uni_key: *const c_char,
    pub vk_code: i32,
    pub sys: i32,
    pub flags: i32,
}

type SelectionCb = extern "C" fn(*const SSelection, *mut c_void);
type MouseCb = extern "C" fn(*const SMouseEvent, *mut c_void);
type KeyCb = extern "C" fn(*const SKeyEvent, *mut c_void);

extern "C" {
    fn sh_create() -> *mut c_void;
    fn sh_destroy(h: *mut c_void);
    fn sh_start(h: *mut c_void, on_sel: SelectionCb, on_mouse: MouseCb, on_key: KeyCb, user: *mut c_void) -> c_int;
    fn sh_stop(h: *mut c_void);
    fn sh_is_running(h: *mut c_void) -> c_int;
    fn sh_set_passive_mode(h: *mut c_void, passive: c_int);
    fn sh_set_clipboard_enabled(h: *mut c_void, enabled: c_int);
    fn sh_enable_mouse_move(h: *mut c_void, enable: c_int);
    fn sh_set_global_filter(h: *mut c_void, mode: c_int, list: *const *const c_char, count: c_int);
    fn sh_set_fine_tuned(h: *mut c_void, list_type: c_int, list: *const *const c_char, count: c_int);
    fn sh_get_current_selection(h: *mut c_void, out: *mut SSelection) -> c_int;
    fn sh_write_clipboard(h: *mut c_void, utf8: *const c_char) -> c_int;
    fn sh_last_error(h: *mut c_void) -> *const c_char;
}

// ---------------------------------------------------------------------------
// Owned payload handed to the app on the Tauri event loop
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct SelectionPayload {
    pub text: String,
    pub program_name: String,
    pub pos_level: i32,
    pub ref_point: SPoint,
    pub mouse_pos_end: SPoint,
    pub start_top: SPoint,
    pub end_bottom: SPoint,
}

static APP: OnceLock<AppHandle> = OnceLock::new();
static HOOK: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static STARTED: AtomicBool = AtomicBool::new(false);
/// Pause automatic reruns while the toolbar/action windows are driving their own flow.
static SUPPRESS: AtomicBool = AtomicBool::new(false);

fn unescape_ptr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

extern "C" fn on_selection(sel: *const SSelection, _user: *mut c_void) {
    if sel.is_null() {
        return;
    }
    let s = unsafe { &*sel };
    // Copy everything now: the pointers are only valid for this call.
    let payload = SelectionPayload {
        text: unescape_ptr(s.text),
        program_name: unescape_ptr(s.program_name),
        pos_level: s.pos_level,
        ref_point: SPoint {
            x: s.mouse_pos_end.x,
            y: s.mouse_pos_end.y,
        },
        mouse_pos_end: s.mouse_pos_end,
        start_top: s.start_top,
        end_bottom: s.end_bottom,
    };
    if payload.text.is_empty() {
        return;
    }
    if let Some(app) = APP.get() {
        let _ = app.emit("selection", payload);
    }
}

extern "C" fn on_mouse(ev: *const SMouseEvent, _user: *mut c_void) {
    if ev.is_null() {
        return;
    }
    let e = unsafe { &*ev };
    // action: 0=down 1=up 2=move 3=wheel
    if e.action == 0 || e.action == 3 {
        if let Some(app) = APP.get() {
            let _ = app.emit("global-pointer", serde_json::json!({ "action": e.action, "x": e.x, "y": e.y }));
        }
    }
    let _ = SUPPRESS;
}

extern "C" fn on_key(ev: *const SKeyEvent, _user: *mut c_void) {
    if ev.is_null() {
        return;
    }
    let e = unsafe { &*ev };
    if e.action == 0 {
        if let Some(app) = APP.get() {
            let _ = app.emit("global-key", serde_json::json!({ "uniKey": unescape_ptr(e.uni_key) }));
        }
    }
}

#[derive(Default, Clone, Deserialize)]
pub struct HookConfig {
    #[serde(default, rename = "triggerMode")]
    pub trigger_mode: Option<String>,
    #[serde(default, rename = "filterMode")]
    pub filter_mode: Option<String>,
    #[serde(default, rename = "filterList")]
    pub filter_list: Option<Vec<String>>,
    #[serde(default, rename = "clipboardFallback")]
    pub clipboard_fallback: Option<bool>,
}

/// Apply configuration to the running hook.
pub fn apply_config(cfg: &HookConfig) {
    let h = HOOK.load(Ordering::SeqCst);
    if h.is_null() {
        return;
    }
    let ctrls: Vec<std::ffi::CString> = cfg
        .filter_list
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| std::ffi::CString::new(s).ok())
        .collect();
    let ptrs: Vec<*const c_char> = ctrls.iter().map(|c| c.as_ptr()).collect();

    let mode = match cfg.filter_mode.as_deref() {
        Some("whitelist") => 1,
        Some("blacklist") => 2,
        _ => 0,
    };
    unsafe {
        sh_set_passive_mode(h, if cfg.trigger_mode.as_deref() == Some("ctrlkey") { 1 } else { 0 });
        sh_set_clipboard_enabled(h, if cfg.clipboard_fallback.unwrap_or(true) { 1 } else { 0 });
        sh_set_global_filter(h, mode, ptrs.as_ptr(), ptrs.len() as c_int);
    }
    // Predefined blacklist for the "selected" trigger mode (mirrors Cherry Studio).
    if cfg.filter_mode.as_deref().unwrap_or("default") == "default" {
        let predef = PREDEFINED_BLACKLIST
            .iter()
            .filter_map(|s| std::ffi::CString::new(*s).ok())
            .collect::<Vec<_>>();
        let pp: Vec<*const c_char> = predef.iter().map(|c| c.as_ptr()).collect();
        unsafe { sh_set_global_filter(h, 2, pp.as_ptr(), pp.len() as c_int) };
    }
    unsafe {
        let ftl = ["acrobat.exe", "wps.exe", "cajviewer.exe"];
        let ftl_c: Vec<std::ffi::CString> = ftl.iter().filter_map(|s| std::ffi::CString::new(*s).ok()).collect();
        let ftl_p: Vec<*const c_char> = ftl_c.iter().map(|c| c.as_ptr()).collect();
        sh_set_fine_tuned(h, 0, ftl_p.as_ptr(), ftl_p.len() as c_int);
    }
}

pub const PREDEFINED_BLACKLIST: &[&str] = &[
    "explorer.exe",
    "snipaste.exe",
    "pixpin.exe",
    "sharex.exe",
    "excel.exe",
    "powerpnt.exe",
    "photoshop.exe",
    "illustrator.exe",
    "adobe premiere pro.exe",
    "afterfx.exe",
    "adobe audition.exe",
    "blender.exe",
    "3dsmax.exe",
    "maya.exe",
    "acad.exe",
    "sldworks.exe",
    "mstsc.exe",
];

pub fn ensure_created() -> bool {
    if !HOOK.load(Ordering::SeqCst).is_null() {
        return true;
    }
    let h = unsafe { sh_create() };
    if h.is_null() {
        return false;
    }
    HOOK.store(h, Ordering::SeqCst);
    true
}

pub fn set_app(app: AppHandle) {
    let _ = APP.set(app);
}

pub fn start(cfg: &HookConfig) -> Result<(), String> {
    if !ensure_created() {
        return Err("sh_create failed".into());
    }
    let h = HOOK.load(Ordering::SeqCst);
    if unsafe { sh_is_running(h) } != 0 {
        apply_config(cfg);
        return Ok(());
    }
    let ok = unsafe { sh_start(h, on_selection, on_mouse, on_key, std::ptr::null_mut()) };
    if ok == 0 {
        let e = unsafe { CStr::from_ptr(sh_last_error(h)) }.to_string_lossy().into_owned();
        return Err(format!("sh_start failed: {e}"));
    }
    STARTED.store(true, Ordering::SeqCst);
    apply_config(cfg);
    Ok(())
}

pub fn stop() {
    let h = HOOK.load(Ordering::SeqCst);
    if !h.is_null() && unsafe { sh_is_running(h) } != 0 {
        unsafe { sh_stop(h) };
    }
    STARTED.store(false, Ordering::SeqCst);
}

pub fn is_running() -> bool {
    let h = HOOK.load(Ordering::SeqCst);
    !h.is_null() && unsafe { sh_is_running(h) } != 0
}

/// Synchronous retrieval (used by the "get current selection" command / hotkey).
pub fn get_current_selection() -> Option<SelectionPayload> {
    let h = HOOK.load(Ordering::SeqCst);
    if h.is_null() {
        return None;
    }
    let mut out: SSelection = unsafe { std::mem::zeroed() };
    if unsafe { sh_get_current_selection(h, &mut out) } == 0 {
        return None;
    }
    let text = unescape_ptr(out.text);
    if text.is_empty() {
        return None;
    }
    Some(SelectionPayload {
        text,
        program_name: unescape_ptr(out.program_name),
        pos_level: out.pos_level,
        ref_point: out.mouse_pos_end,
        mouse_pos_end: out.mouse_pos_end,
        start_top: out.start_top,
        end_bottom: out.end_bottom,
    })
}

pub fn write_clipboard(text: &str) -> bool {
    let h = HOOK.load(Ordering::SeqCst);
    if h.is_null() {
        return false;
    }
    match std::ffi::CString::new(text) {
        Ok(c) => unsafe { sh_write_clipboard(h, c.as_ptr()) != 0 },
        Err(_) => false,
    }
}

pub fn cleanup() {
    let h = HOOK.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !h.is_null() {
        unsafe {
            sh_stop(h);
            sh_destroy(h);
        }
    }
}
