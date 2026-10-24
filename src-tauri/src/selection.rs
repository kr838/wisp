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
//! Retrieval is always requested asynchronously (`request_selection`): the worker queues it
//! and reports back through the selection callback. Calling UIAutomation from the UI thread
//! is what froze the app when the target application was busy.

use std::ffi::{c_char, c_void, CStr};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{OnceLock};

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

/// Resolved entry points of `SelectionHook.dll`.
///
/// The DLL is loaded at runtime instead of being imported at link time. Two reasons:
///
/// * `cargo test --lib` links the library into a test binary whose working directory is the
///   build's `deps/` folder. With a link-time import, the DLL has to be present there too or
///   Windows aborts the process at load with `STATUS_ENTRYPOINT_NOT_FOUND` before a single
///   test runs — which looks exactly like a code failure.
/// * The import library carries a static libgcc unwinder, so the linker had to be told to
///   tolerate duplicate `_Unwind_Resume` symbols.
///
/// Loading with `LoadLibraryW` keeps the dependency entirely out of the executable's import
/// table, and lets a missing DLL be reported as a readable error instead of a process abort.
struct Api {
    create: unsafe extern "C" fn() -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    start: unsafe extern "C" fn(*mut c_void, SelectionCb, MouseCb, KeyCb, *mut c_void) -> c_int,
    stop: unsafe extern "C" fn(*mut c_void),
    is_running: unsafe extern "C" fn(*mut c_void) -> c_int,
    set_passive_mode: unsafe extern "C" fn(*mut c_void, c_int),
    set_clipboard_enabled: unsafe extern "C" fn(*mut c_void, c_int),
    set_global_filter: unsafe extern "C" fn(*mut c_void, c_int, *const *const c_char, c_int),
    set_fine_tuned: unsafe extern "C" fn(*mut c_void, c_int, *const *const c_char, c_int),
    request_selection: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    write_clipboard: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
    last_error: unsafe extern "C" fn(*mut c_void) -> *const c_char,
}

// The function pointers are immutable after `OnceLock` initialisation and the DLL stays
// loaded for the whole process lifetime.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn dll_candidates() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("SelectionHook.dll"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        v.push(cwd.join("SelectionHook.dll"));
        // Common dev layouts: running from src-tauri/ or the repo root.
        v.push(cwd.join("native").join("SelectionHook.dll"));
        v.push(cwd.join("..").join("native").join("SelectionHook.dll"));
    }
    v
}

#[cfg(windows)]
fn load_api() -> Result<Api, String> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    let mut last_err = String::from("找不到 SelectionHook.dll");
    for path in dll_candidates() {
        if !path.exists() {
            continue;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            last_err = format!("无法加载 {}", path.display());
            continue;
        }
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let p = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr() as *const c_char) };
                if p.is_null() {
                    return Err(format!("{} 缺少导出 {}", path.display(), $name));
                }
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
            }};
        }
        return Ok(Api {
            create: sym!("sh_create", unsafe extern "C" fn() -> *mut c_void),
            destroy: sym!("sh_destroy", unsafe extern "C" fn(*mut c_void)),
            start: sym!(
                "sh_start",
                unsafe extern "C" fn(*mut c_void, SelectionCb, MouseCb, KeyCb, *mut c_void) -> c_int
            ),
            stop: sym!("sh_stop", unsafe extern "C" fn(*mut c_void)),
            is_running: sym!("sh_is_running", unsafe extern "C" fn(*mut c_void) -> c_int),
            set_passive_mode: sym!("sh_set_passive_mode", unsafe extern "C" fn(*mut c_void, c_int)),
            set_clipboard_enabled: sym!("sh_set_clipboard_enabled", unsafe extern "C" fn(*mut c_void, c_int)),
            set_global_filter: sym!(
                "sh_set_global_filter",
                unsafe extern "C" fn(*mut c_void, c_int, *const *const c_char, c_int)
            ),
            set_fine_tuned: sym!(
                "sh_set_fine_tuned",
                unsafe extern "C" fn(*mut c_void, c_int, *const *const c_char, c_int)
            ),
            request_selection: sym!("sh_request_selection", unsafe extern "C" fn(*mut c_void, c_int) -> c_int),
            write_clipboard: sym!(
                "sh_write_clipboard",
                unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int
            ),
            last_error: sym!("sh_last_error", unsafe extern "C" fn(*mut c_void) -> *const c_char),
        });
    }
    Err(last_err)
}

#[cfg(not(windows))]
fn load_api() -> Result<Api, String> {
    Err("selection engine is only available on Windows".into())
}

fn api() -> Result<&'static Api, String> {
    match API.get_or_init(load_api) {
        Ok(a) => Ok(a),
        Err(e) => Err(e.clone()),
    }
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
    // Only the two events the app reacts to are forwarded (0=down, 3=wheel).
    if e.action == 0 || e.action == 3 {
        if let Some(app) = APP.get() {
            let _ = app.emit("global-pointer", serde_json::json!({ "action": e.action, "x": e.x, "y": e.y }));
        }
    }
}

extern "C" fn on_key(ev: *const SKeyEvent, _user: *mut c_void) {
    if ev.is_null() {
        return;
    }
    let e = unsafe { &*ev };
    // Both edges are forwarded: the hold-Ctrl trigger needs key-up to reset its state.
    if let Some(app) = APP.get() {
        let _ = app.emit(
            "global-key",
            serde_json::json!({
                "uniKey": unescape_ptr(e.uni_key),
                "action": e.action,
                "vkCode": e.vk_code,
                "flags": e.flags,
            }),
        );
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
    let Ok(api) = api() else { return };
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
    // In "hold Ctrl" mode gesture detection is off: the toolbar is shown by the key
    // watcher in `lib.rs` instead.
    let passive = cfg.trigger_mode.as_deref() == Some("ctrlkey");
    unsafe {
        (api.set_passive_mode)(h, if passive { 1 } else { 0 });
        (api.set_clipboard_enabled)(h, if cfg.clipboard_fallback.unwrap_or(true) { 1 } else { 0 });
    }

    // Predefined blacklist for the "selected" trigger mode (mirrors Cherry Studio). In Ctrl
    // mode there are no automatic gestures, so no filter is applied — the user's explicit
    // request must always be honoured.
    let default_mode = cfg.filter_mode.as_deref().unwrap_or("default") == "default";
    if default_mode && !passive {
        let predef = PREDEFINED_BLACKLIST
            .iter()
            .filter_map(|s| std::ffi::CString::new(*s).ok())
            .collect::<Vec<_>>();
        let pp: Vec<*const c_char> = predef.iter().map(|c| c.as_ptr()).collect();
        unsafe { (api.set_global_filter)(h, 2, pp.as_ptr(), pp.len() as c_int) };
    } else if default_mode {
        unsafe { (api.set_global_filter)(h, 0, std::ptr::null(), 0) };
    } else {
        unsafe { (api.set_global_filter)(h, mode, ptrs.as_ptr(), ptrs.len() as c_int) };
    }

    unsafe {
        let ftl = ["acrobat.exe", "wps.exe", "cajviewer.exe"];
        let ftl_c: Vec<std::ffi::CString> = ftl.iter().filter_map(|s| std::ffi::CString::new(*s).ok()).collect();
        let ftl_p: Vec<*const c_char> = ftl_c.iter().map(|c| c.as_ptr()).collect();
        (api.set_fine_tuned)(h, 0, ftl_p.as_ptr(), ftl_p.len() as c_int);
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
    let Ok(api) = api() else { return false };
    let h = unsafe { (api.create)() };
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
    let api = api()?;
    if !ensure_created() {
        return Err("选词引擎初始化失败（SelectionHook.dll 不可用）".into());
    }
    let h = HOOK.load(Ordering::SeqCst);
    if unsafe { (api.is_running)(h) } != 0 {
        apply_config(cfg);
        return Ok(());
    }
    let ok = unsafe { (api.start)(h, on_selection, on_mouse, on_key, std::ptr::null_mut()) };
    if ok == 0 {
        let e = unsafe { CStr::from_ptr((api.last_error)(h)) }.to_string_lossy().into_owned();
        return Err(format!("选词引擎启动失败: {e}"));
    }
    apply_config(cfg);
    Ok(())
}

pub fn stop() {
    let h = HOOK.load(Ordering::SeqCst);
    let Ok(api) = api() else { return };
    if !h.is_null() && unsafe { (api.is_running)(h) } != 0 {
        unsafe { (api.stop)(h) };
    }
}

pub fn is_running() -> bool {
    let h = HOOK.load(Ordering::SeqCst);
    if h.is_null() {
        return false;
    }
    let Ok(api) = api() else { return false };
    let running = unsafe { (api.is_running)(h) } != 0;
    running
}

/// Queue an asynchronous read of the current selection.
///
/// Returns false when nothing was queued (hook stopped, no foreground window, or a retrieval
/// is already running). The result arrives through the `selection` Tauri event.
pub fn request_selection(triggered_by_user: bool) -> bool {
    let h = HOOK.load(Ordering::SeqCst);
    if h.is_null() {
        return false;
    }
    let Ok(api) = api() else { return false };
    unsafe { (api.request_selection)(h, if triggered_by_user { 1 } else { 0 }) != 0 }
}

pub fn write_clipboard(text: &str) -> bool {
    let h = HOOK.load(Ordering::SeqCst);
    if h.is_null() {
        return false;
    }
    let Ok(api) = api() else { return false };
    match std::ffi::CString::new(text) {
        Ok(c) => unsafe { (api.write_clipboard)(h, c.as_ptr()) != 0 },
        Err(_) => false,
    }
}

pub fn cleanup() {
    let h = HOOK.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if h.is_null() {
        return;
    }
    let Ok(api) = api() else { return };
    unsafe {
        (api.stop)(h);
        (api.destroy)(h);
    }
}
