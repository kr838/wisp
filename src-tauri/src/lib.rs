//! Wisp — a ~2 MB Windows selection assistant.
//!
//! The original C++ selection engine is reused through a C ABI DLL (`selection.rs`), and the
//! UI is plain HTML/CSS/JS rendered by the system WebView2 (`ui/`). This module owns the
//! windows, the tray, the IPC surface and the app lifecycle.
//!
//! # Window model
//!
//! | window    | role                                   | topmost | size            |
//! |-----------|----------------------------------------|---------|-----------------|
//! | `toolbar` | floating action bar near the selection | yes     | content-sized   |
//! | `action`  | streaming result panel                 | yes     | resizable       |
//! | `settings`| configuration (native frame)           | no      | fixed           |
//!
//! Only the two floating windows are always-on-top; the settings window is a normal window so
//! the user can switch away from it without minimising.
//!
//! # Sizing / DPI note
//!
//! Three ratios exist and are NOT equal: window logical -> physical (`scale_factor`), physical
//! -> CSS (`devicePixelRatio`), and therefore logical -> CSS. Sizes are derived as
//! `physical = CSS * devicePixelRatio`; see `css_to_logical` and `reveal_toolbar`. Sizes are
//! never read back from the window right after setting them, because the resize applies
//! asynchronously and the stale value misplaces the toolbar.

mod ai;
mod selection;
mod store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_opener::OpenerExt;

#[derive(Clone, serde::Serialize)]
struct PendingAction {
    action: Value,
    text: String,
    #[serde(rename = "programName")]
    program_name: String,
    #[serde(rename = "systemPrompt")]
    system_prompt: String,
    provider: Option<Value>,
    #[serde(rename = "modelName")]
    model_name: String,
    #[serde(rename = "thinkingLevel")]
    thinking_level: String,
    #[serde(rename = "thinkingSupported")]
    thinking_supported: bool,
}

struct State {
    last_selection: Mutex<Option<selection::SelectionPayload>>,
    pending: Mutex<Option<PendingAction>>,
    streaming: AtomicBool,
    toolbar_width: Mutex<f64>,
    /// The webview's own device pixel ratio (from the renderer). It can differ from the
    /// window's `scale_factor`, and window sizing must use this one.
    toolbar_dpr: Mutex<f64>,
    /// CSS size we last asked the window to become, used to derive the true
    /// logical->viewport conversion (which is not reliably derivable from devicePixelRatio).
    toolbar_last_req: Mutex<(f64, f64)>,


    /// Anchor (center_x, top_y, bottom_y) of the current toolbar placement, so the
    /// window can be positioned once its content has rendered and width is known.
    toolbar_anchor: Mutex<Option<(f64, f64, f64)>>,
    /// True while the toolbar content is being rendered off-screen; it is shown by
    /// `toolbar_report_size` once the real size is known (avoids a visible size jump).
    show_pending: AtomicBool,
}

impl Default for State {
    fn default() -> Self {
        State {
            last_selection: Mutex::new(None),
            pending: Mutex::new(None),
            streaming: AtomicBool::new(false),
            toolbar_width: Mutex::new(360.0),
            toolbar_dpr: Mutex::new(1.0),
            toolbar_last_req: Mutex::new((0.0, 0.0)),


            toolbar_anchor: Mutex::new(None),
            show_pending: AtomicBool::new(false),
        }
    }
}

/// Preview mode (`--preview`): keep the toolbar visible instead of hiding it on blur, so
/// the UI can be inspected without a real selection.
static PREVIEW: AtomicBool = AtomicBool::new(false);

fn hook_config(s: &store::Settings) -> selection::HookConfig {
    selection::HookConfig {
        trigger_mode: Some(s.trigger_mode.clone()),
        filter_mode: Some(s.filter_mode.clone()),
        filter_list: Some(s.filter_list.clone()),
        clipboard_fallback: Some(s.clipboard_fallback),
    }
}

/// Diagnostic logging.
///
/// The app runs in the GUI subsystem, so stdout/stderr go nowhere. Set `WISP_DEBUG=1`
/// to append diagnostics to `%TEMP%\wisp-debug.log`.
pub fn dlog(msg: &str) {
    if std::env::var("WISP_DEBUG").is_err() {
        return;
    }
    use std::io::Write;
    let path = std::env::temp_dir().join("wisp-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

#[macro_export]
macro_rules! dlogf {
    ($($arg:tt)*) => { $crate::dlog(&format!($($arg)*)) };
}

fn settings_json(s: &store::Settings) -> Value {
    json!({
        "enabled": s.enabled,
        "triggerMode": s.trigger_mode,
        "followToolbar": s.follow_toolbar,
        "clipboardFallback": s.clipboard_fallback,
        "filterMode": s.filter_mode,
        "filterList": s.filter_list,
        "theme": s.theme,
        "material": s.material,
        "glassOpacity": s.glass_opacity,
        "materialDark": s.material_dark,
        "searchUrl": s.search_url,
        "thinkingLevel": s.thinking_level,
        "actions": s.actions,
        "providers": s.providers,
        "defaultProviderId": s.default_provider_id,
        "hookRunning": selection::is_running(),
        "userDataPath": store::path().to_string_lossy(),
    })
}

fn pick_provider(s: &store::Settings, action_provider: Option<&str>) -> Option<store::Provider> {
    let usable: Vec<&store::Provider> = s
        .providers
        .iter()
        .filter(|p| !p.api_base.trim().is_empty() && !p.model.trim().is_empty())
        .collect();
    if usable.is_empty() {
        return None;
    }
    if let Some(id) = action_provider {
        if let Some(p) = usable.iter().find(|p| p.id == id) {
            return Some((*p).clone());
        }
    }
    if !s.default_provider_id.is_empty() {
        if let Some(p) = usable.iter().find(|p| p.id == s.default_provider_id) {
            return Some((*p).clone());
        }
    }
    Some(usable[0].clone())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn settings_get() -> Value {
    settings_json(&store::load())
}

/// Appearance values the windows need to paint themselves. Kept separate from
/// `settings_get` so the toolbar/result windows can fetch just this on every open (the
/// material and tint can change while they are hidden).
#[tauri::command]
fn ui_prefs() -> Value {
    let s = store::load();
    json!({
        "theme": s.theme,
        "material": s.material,
        "glassOpacity": s.glass_opacity,
        "compact": s.compact,
    })
}

#[tauri::command]
fn app_info() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "name": "Wisp",
        "hookRunning": selection::is_running(),
        "userDataPath": store::path().to_string_lossy(),
    })
}

#[tauri::command]
fn settings_set(app: AppHandle, patch: Value) -> Value {
    let prev = store::load();
    let next = store::merge(&patch);
    if prev.enabled != next.enabled {
        if next.enabled {
            let _ = selection::start(&hook_config(&next));
        } else {
            selection::stop();
        }
    } else {
        selection::apply_config(&hook_config(&next));
    }
    rebuild_tray(&app, &next);
    // Material / tint changes are native, so they must be pushed to the live windows.
    apply_material_all(&app);
    let _ = app.emit("settings-changed", settings_json(&next));
    settings_json(&next)
}

#[tauri::command]
fn settings_reset(app: AppHandle) -> Value {
    let s = store::reset();
    let _ = selection::start(&hook_config(&s));
    rebuild_tray(&app, &s);
    settings_json(&s)
}

#[tauri::command]
async fn test_provider(provider: store::Provider) -> Value {
    let (ok, msg) = ai::test_provider(&provider).await;
    json!({ "ok": ok, "message": msg })
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    show_settings(&app);
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) {
    let _ = app.opener().open_url(url, None::<&str>);
}

fn hide_toolbar(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("toolbar") {
        if w.is_visible().unwrap_or(false) {
            crate::dlogf!("[app] toolbar hidden");
            let _ = w.hide();
        }
    }
}

#[tauri::command]
fn toolbar_hide(app: AppHandle) {
    hide_toolbar(&app);
}

#[tauri::command]
fn toolbar_get_state() -> Value {
    let s = store::load();
    let prov = pick_provider(&s, None);
    let level = prov
        .as_ref()
        .map(|p| ai::resolve_level(p, None, &s.thinking_level))
        .unwrap_or_else(|| "auto".into());
    json!({
        "actions": s.actions.iter().filter(|a| a.enabled).collect::<Vec<_>>(),
        "theme": s.theme,
        "compact": s.compact,
        "hasProvider": prov.is_some(),
        "thinkingLevel": level,
    })
}

#[tauri::command]
fn toolbar_copy(app: AppHandle, text: String) {
    selection::write_clipboard(&text);
    hide_toolbar(&app);
}

#[tauri::command]
fn toolbar_search(app: AppHandle, text: String) {
    let s = store::load();
    let q = urlencode(&text);
    let url = s.search_url.replace("{query}", &q);
    let _ = app.opener().open_url(url, None::<&str>);
    hide_toolbar(&app);
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "%20".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tauri::command]
fn run_action(app: AppHandle, action_id: String) -> Value {
    let s = store::load();
    let action = s
        .actions
        .iter()
        .find(|a| a.id == action_id)
        .cloned()
        .unwrap_or(store::Action {
            id: action_id.clone(),
            name: "处理".into(),
            color: store::RAINBOW[0].into(),
            icon: String::new(),
            builtin: "custom".into(),
            prompt: String::new(),
            enabled: true,
            thinking_level: "inherit".into(),
            provider_id: None,
        });
    let sel = {
        let guard = app.state::<State>();
        let g = guard.last_selection.lock().unwrap();
        g.clone()
    };
    let Some(sel) = sel else {
        crate::dlogf!("[app] run_action {action_id}: no selection");
        return json!({ "ok": false, "error": "没有选中文本" });
    };

    if action.builtin == "copy" {
        selection::write_clipboard(&sel.text);
        hide_toolbar(&app);
        return json!({ "ok": true, "local": true });
    }

    // Search is a normal (default-off) action: open the configured engine with the
    // selected text. Keeping it in the action list rather than hardcoding a button is what
    // lets the user enable/rename/reorder it from 设置 → 动作.
    if action.builtin == "search" {
        let q = urlencode(&sel.text);
        let url = s.search_url.replace("{query}", &q);
        let _ = app.opener().open_url(url, None::<&str>);
        hide_toolbar(&app);
        return json!({ "ok": true, "local": true });
    }

    let provider = pick_provider(&s, action.provider_id.as_deref());
    crate::dlogf!(
        "[app] run_action {} textLen={} provider={:?}",
        action_id,
        sel.text.chars().count(),
        provider.as_ref().map(|p| p.model.clone())
    );
    let level = provider
        .as_ref()
        .map(|p| ai::resolve_level(p, Some(&action.thinking_level), &s.thinking_level))
        .unwrap_or_else(|| "auto".into());
    let supported = provider.as_ref().map(ai::supports_thinking).unwrap_or(false);

    let pending = PendingAction {
        action: json!({ "id": action.id, "name": action.name, "icon": action.icon, "builtin": action.builtin }),
        text: sel.text.clone(),
        program_name: sel.program_name.clone(),
        system_prompt: ai::system_prompt(&action.builtin, &action.prompt),
        model_name: provider.as_ref().map(|p| p.model.clone()).unwrap_or_default(),
        provider: provider.as_ref().map(|p| serde_json::to_value(p).unwrap_or(json!({}))),
        thinking_level: level,
        thinking_supported: supported,
    };
    *app.state::<State>().pending.lock().unwrap() = Some(pending.clone());

    hide_toolbar(&app);
    show_action(&app);
    let _ = app.emit_to("action", "action-start", pending);
    crate::dlogf!("[app] run_action {} -> action window shown", action_id);
    json!({ "ok": true })
}

#[tauri::command]
fn action_get_context(app: AppHandle) -> Value {
    let guard = app.state::<State>();
    let g = guard.pending.lock().unwrap();
    match g.as_ref() {
        Some(p) => serde_json::to_value(p).unwrap_or(json!(null)),
        None => json!(null),
    }
}

#[tauri::command]
fn action_close(app: AppHandle) {
    if let Some(w) = app.get_webview_window("action") {
        let _ = w.hide();
    }
}

#[tauri::command]
fn action_stop(app: AppHandle) {
    app.state::<State>().streaming.store(false, Ordering::SeqCst);
    let _ = app.emit_to("action", "action-aborted", ());
}

#[tauri::command]
fn action_copy(text: String) {
    selection::write_clipboard(&text);
}

#[tauri::command]
fn action_run(app: AppHandle) {
    let pending = {
        let guard = app.state::<State>();
        let g = guard.pending.lock().unwrap();
        g.clone()
    };
    let Some(p) = pending else {
        crate::dlogf!("[app] action_run: no pending action");
        let _ = app.emit_to("action", "action-error", "没有可执行的动作");
        return;
    };
    let Some(prov_val) = p.provider.clone() else {
        crate::dlogf!("[app] action_run: no provider configured");
        let _ = app.emit_to("action", "action-error", "未配置 AI 模型，请先在设置中添加模型提供商");
        return;
    };
    let provider: store::Provider = serde_json::from_value(prov_val).unwrap_or_default();

    app.state::<State>().streaming.store(true, Ordering::SeqCst);
    let app2 = app.clone();
    crate::dlogf!(
        "[app] action_run model={} level={} userLen={}",
        provider.model, p.thinking_level, p.text.chars().count()
    );
    tauri::async_runtime::spawn(async move {
        let app_chunk = app2.clone();
        let app_reason = app2.clone();
        let res = ai::chat_stream(
            &provider,
            &p.system_prompt,
            &p.text,
            &p.thinking_level,
            move |t| {
                let _ = app_chunk.emit_to("action", "action-chunk", t.to_string());
            },
            move |t| {
                let _ = app_reason.emit_to("action", "action-reasoning", t.to_string());
            },
        )
        .await;

        let streaming = app2.state::<State>().streaming.load(Ordering::SeqCst);
        match res {
            Ok(out) => {
                crate::dlogf!("[app] action_run done, textLen={}", out.text.chars().count());
                if streaming {
                    let _ = app2.emit_to("action", "action-done", out.text);
                }
            }
            Err(e) => {
                crate::dlogf!("[app] action_run error: {e}");
                if streaming {
                    let _ = app2.emit_to("action", "action-error", e);
                }
            }
        }
        app2.state::<State>().streaming.store(false, Ordering::SeqCst);
    });
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

fn build_toolbar(app: &AppHandle) {
    if app.get_webview_window("toolbar").is_some() {
        return;
    }
    // focusable(false) sets WS_EX_NOACTIVATE: the toolbar still receives mouse clicks but
    // never takes keyboard focus, so the user keeps typing / using Ctrl+C in their own app.
    // Cherry Studio's toolbar does exactly this on Windows.
    let built = WebviewWindowBuilder::new(app, "toolbar", WebviewUrl::App("toolbar/index.html".into()))
        .title("Wisp")
        .inner_size(MEASURE_WIDTH, TOOLBAR_HEIGHT)
        .position(-10000.0, -10000.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        // No OS shadow: it brings an invisible resize frame that eats into the client
        // area and would clip the last chip. The CSS box-shadow provides the shadow
        // instead — the same choice Cherry Studio's toolbar window makes.
        .shadow(false)
        .visible(false)
        .build();
    if let Ok(w) = built {
        apply_frosted_glass(&w);
    }
}

/// Give the window a real frosted-glass (acrylic) backdrop with rounded corners.
///
/// CSS `backdrop-filter` cannot blur the desktop behind a separate window, so the blur
/// has to come from the OS. Rounding the window itself (rather than drawing a rounded
/// pill inside a rectangular window) is what prevents the "rounded card floating in a
/// translucent rectangle" look: the acrylic region then *is* the pill.
fn apply_frosted_glass(w: &tauri::WebviewWindow) {
    apply_material(w, &store::load());
}

/// Apply the configured backdrop material to a window.
///
/// The three materials differ in what they sample, which is the practical difference the
/// user sees:
///   - Mica (`DWMSBT_MAINWINDOW`) samples the **desktop wallpaper**; it barely blurs and
///     therefore looks flat when the window floats over another app.
///   - Mica Alt (`DWMSBT_TABBEDWINDOW`) is a higher-contrast variant of the same idea.
///   - Acrylic (`DWMSBT_TRANSIENTWINDOW`) is real-time blur of **whatever is behind the
///     window** — the classic frosted glass.
fn apply_material(w: &tauri::WebviewWindow, s: &store::Settings) {
    use window_vibrancy::{apply_acrylic, apply_mica, apply_tabbed, clear_acrylic, clear_mica};

    if w.hwnd().is_err() {
        return;
    }
    // None = follow the system light/dark setting.
    let dark = match s.material_dark.as_str() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    };

    // Clear the previous backdrop first so switching materials cannot stack two of them.
    let _ = clear_mica(w);
    let _ = clear_acrylic(w);

    match s.material.as_str() {
        "none" => {}
        "acrylic" => {
            // Acrylic needs an explicit tint; pick one that matches the requested variant.
            let tint = if dark.unwrap_or(false) { (28, 28, 30, 150) } else { (246, 247, 252, 150) };
            let _ = apply_acrylic(w, Some(tint));
        }
        "mica_alt" => {
            let _ = apply_tabbed(w, dark);
        }
        // "mica" and anything unknown
        _ => {
            if apply_mica(w, dark).is_err() {
                // Windows 10 has no Mica; acrylic is the closest available fallback.
                let _ = apply_acrylic(w, Some((246, 247, 252, 150)));
            }
        }
    }
    round_window_corners(w);
}

/// Re-apply the material to every window that uses one. Called at startup and whenever the
/// material settings change, so the switch is live rather than requiring a restart.
fn apply_material_all(app: &AppHandle) {
    let s = store::load();
    for label in ["toolbar", "action"] {
        if let Some(w) = app.get_webview_window(label) {
            apply_material(&w, &s);
        }
    }
}

/// Ask DWM to round the window corners (Windows 11). Ignored on Windows 10.
fn round_window_corners(w: &tauri::WebviewWindow) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;
    if let Ok(handle) = w.hwnd() {
        unsafe {
            DwmSetWindowAttribute(
                handle.0 as isize,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &DWMWCP_ROUND as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

fn build_action(app: &AppHandle) {
    if app.get_webview_window("action").is_some() {
        return;
    }
    // One surface only: the window itself is the frosted panel (acrylic/mica + rounded
    // corners). The webview paints no background and no inset card, so there is no second
    // layer and no translucent frame around the content.
    let built = WebviewWindowBuilder::new(app, "action", WebviewUrl::App("action/index.html".into()))
        .title("Wisp")
        .inner_size(480.0, 420.0)
        .min_inner_size(360.0, 220.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(false)
        .resizable(true)
        // The OS shadow draws around the whole transparent rectangle, not the rounded
        // panel, which is what produced the "big transparent frame" look. The panel draws
        // its own shadow instead.
        .shadow(false)
        .visible(false)
        .build();
    if let Ok(w) = built {
        apply_frosted_glass(&w);
    }
}

/// Show the settings window.
///
/// The result window is always-on-top and centred, so without this the settings window
/// opens *underneath* it and looks like nothing happened. The settings window is therefore
/// also made always-on-top, and the result window is hidden while it is used.
fn show_settings(app: &AppHandle) {
    // Clear the floating windows out of the way so the settings window is not buried —
    // without making settings itself always-on-top, which would keep it above every other
    // application until the user minimised it.
    for label in ["action", "toolbar"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    }
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        // Explicitly clear a topmost flag that an earlier build may have set; otherwise the
        // window would stay pinned above other apps for this session.
        let _ = w.set_always_on_top(false);
        let _ = w.show();
        let _ = w.set_focus();
        crate::dlogf!("[app] settings window re-shown");
        return;
    }
    match WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("Wisp · 设置")
        .inner_size(880.0, 660.0)
        .min_inner_size(720.0, 520.0)
        .center()
        // Deliberately NOT always-on-top: settings is a normal window the user switches to
        // and away from. Only the floating toolbar and the result popup belong on top.
        .always_on_top(false)
        .build()
    {
        Ok(w) => {
            let _ = w.set_focus();
            crate::dlogf!("[app] settings window created");
        }
        Err(e) => crate::dlogf!("[app] settings window FAILED: {e}"),
    }
}

fn show_action(app: &AppHandle) {
    if app.get_webview_window("action").is_none() {
        build_action(app);
    }
    if let Some(w) = app.get_webview_window("action") {
        position_action(app, &w);
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn position_action(app: &AppHandle, w: &tauri::WebviewWindow) {
    let s = store::load();
    if s.follow_toolbar {
        if let Some(tb) = app.get_webview_window("toolbar") {
            // Use logical (DIP) coordinates to stay consistent with the toolbar window.
            if let (Ok(tbpos), Ok(tbsize)) = (tb.outer_position(), tb.outer_size()) {
                let scale = tb.scale_factor().unwrap_or(1.0).max(0.1);
                let (x, y) = (tbpos.x as f64 / scale, tbpos.y as f64 / scale);
                let h = tbsize.height as f64 / scale;
                let _ = w.set_position(tauri::LogicalPosition::new(x, y + h + 6.0));
                return;
            }
        }
    }
    let _ = w.center();
}

/// Show the floating toolbar for a selection.
///
/// The window is *not* shown here: its content is rendered while hidden and the real
/// size comes back through `toolbar_report_size`, which then positions and reveals it.
/// Showing first would display the previous (wrong) width for a frame.
fn show_toolbar_at(app: &AppHandle, payload: &selection::SelectionPayload) {
    if app.get_webview_window("toolbar").is_none() {
        build_toolbar(app);
    }
    let Some(w) = app.get_webview_window("toolbar") else { return };

    // The low-level hook reports *physical* pixels. The anchor is stored raw here and
    // converted to logical pixels (DIP) in reveal_toolbar, where the window's scale
    // factor is known to be settled (Cherry Studio does the same screenToDipPoint step).
    // The clipboard fallback (Win32 edit controls and similar) returns text without
    // coordinates, so pos_level is 0 and every point is (0,0). Fall back to the live
    // cursor position — the same thing Cherry Studio does for PositionLevel::NONE.
    let no_position = payload.pos_level <= 0
        || (payload.ref_point.x == 0 && payload.ref_point.y == 0 && payload.mouse_pos_end.x == 0);

    let (anchor_x, below_y, anchor_bottom) = if no_position {
        let (px, py) = cursor_position();
        (px as f64, py as f64 + 24.0, py as f64)
    } else {
        match payload.pos_level {
            3 | 4 => (
                payload.end_bottom.x as f64,
                payload.end_bottom.y as f64 + 10.0,
                payload.end_bottom.y as f64,
            ),
            _ => (
                payload.mouse_pos_end.x as f64,
                payload.mouse_pos_end.y as f64 + 24.0,
                payload.mouse_pos_end.y as f64,
            ),
        }
    };

    *app.state::<State>().toolbar_anchor.lock().unwrap() = Some((anchor_x, below_y, anchor_bottom));
    app.state::<State>().show_pending.store(true, Ordering::SeqCst);

    // Pre-widen the (still hidden) window before the renderer measures. A window narrower
    // than the content compresses the row, so the reported width would be too small and the
    // last action would be clipped — the measurement must be unconstrained. Reuse the last
    // known width when it is larger, so the window never has to be stretched beyond the
    // screen (which would make the renderer report a clipped width).
    // Stretch to (almost) the monitor width in DIP: wide enough that the row is never
    // compressed, yet still on-screen so the webview does not clip the content and report
    // a wrong width.
    let last = *app.state::<State>().toolbar_width.lock().unwrap();
    let screen_dip = w
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.size().width as f64 / m.scale_factor().max(0.1))
        .unwrap_or(1200.0);
    let measure_w = (screen_dip - 40.0).max(last).max(MEASURE_WIDTH_MIN).min(MEASURE_WIDTH);
    let mdpr = {
        let d = *app.state::<State>().toolbar_dpr.lock().unwrap();
        if d > 0.5 { d } else { w.scale_factor().unwrap_or(1.0).max(0.5) }
    };
    let mscale = w.scale_factor().unwrap_or(1.0).max(0.1);
    let lw = css_to_logical(measure_w, mdpr, mscale);
    let lh = css_to_logical(TOOLBAR_HEIGHT, mdpr, mscale);
    let _ = w.set_size(tauri::LogicalSize::new(lw, lh));
    *app.state::<State>().toolbar_last_req.lock().unwrap() = (lw, lh);
    let _ = app.emit_to("toolbar", "toolbar-payload", payload.clone());

    // Fallback: if the renderer never reports a size, reveal the toolbar anyway so a
    // front-end error cannot make selection appear to do nothing.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        if app2.state::<State>().show_pending.swap(false, Ordering::SeqCst) {
            reveal_toolbar(&app2, *app2.state::<State>().toolbar_width.lock().unwrap());
        }
    });
    let _ = w;
}

/// Fixed toolbar height, matching Cherry Studio: the pill is 36px plus 2px top and
/// 5px bottom margin. The window height never changes; only the width is measured.
const TOOLBAR_HEIGHT: f64 = 36.0;

/// Width the hidden toolbar is stretched to while it measures its content. It must be
/// wider than the tallest expected row, but is capped to the screen width so the window
/// never extends off-screen (a clipped window makes the renderer under-report).
const MEASURE_WIDTH: f64 = 900.0;
/// Lower bound when reusing the previous width for the measuring pass.
const MEASURE_WIDTH_MIN: f64 = 360.0;

/// Position and show the toolbar using a known content width (CSS pixels == DIP).
/// The stored anchor is in physical pixels and is converted here.
fn reveal_toolbar(app: &AppHandle, width: f64) {
    let Some(w) = app.get_webview_window("toolbar") else { return };
    let anchor = *app.state::<State>().toolbar_anchor.lock().unwrap();
    let Some((anchor_x_px, below_y_px, anchor_bottom_px)) = anchor else { return };

    // Sizing: physical = CSS * devicePixelRatio. Measured here: inner_size was 1344 physical
    // while innerWidth was 750 CSS with scale_factor 1.5 — so the window's logical units are
    // NOT the webview's CSS units (1.5 vs 1.8), and passing a CSS size as LogicalSize yields
    // only ~83% of the intended viewport. devicePixelRatio is the ratio that is actually
    // consistent with the physical/CSS relation, so derive both the size and the position
    // from it.
    //
    // Nothing is read back from the window: a requested resize applies asynchronously, so
    // `outer_size()` immediately after `set_size` still returns the previous size (observed:
    // read 1620 while requesting 304), which placed the bar hundreds of px off-screen.
    let dpr = {
        let d = *app.state::<State>().toolbar_dpr.lock().unwrap();
        if d > 0.5 {
            d
        } else {
            w.scale_factor().unwrap_or(1.0).max(0.5)
        }
    };
    let css_w = width.max(120.0);
    let w_px = (css_w * dpr).round();
    let h_px = (TOOLBAR_HEIGHT * dpr).round();

    let scale = w.scale_factor().unwrap_or(1.0).max(0.1);
    let logical_w = css_to_logical(css_w, dpr, scale);
    let logical_h = css_to_logical(TOOLBAR_HEIGHT, dpr, scale);
    *app.state::<State>().toolbar_last_req.lock().unwrap() = (logical_w, logical_h);
    let _ = w.set_size(tauri::LogicalSize::new(logical_w, logical_h));

    let (anchor_x, below_y, anchor_bottom) = (anchor_x_px, below_y_px, anchor_bottom_px);
    let mut x = anchor_x - w_px / 2.0;
    let mut y = below_y;
    // Clamp to the monitor containing the anchor, in physical pixels.
    if let Some((mx, my, mw, mh)) = monitor_phys_at(app, anchor_x, below_y) {
        let margin = 8.0;
        let max_x = mx + mw - w_px - margin;
        let max_y = my + mh - h_px - margin;
        if x > max_x {
            x = max_x;
        }
        if y > max_y {
            // Not enough room below: place it above the anchor.
            y = (anchor_bottom - h_px - margin).max(my + margin);
        }
        x = x.max(mx + margin);
        y = y.max(my + margin);
    }
    let _ = w.set_position(tauri::PhysicalPosition::new(x.round() as i32, y.round() as i32));
    let _ = w.show();
    // Deliberately no set_focus(): the window is WS_EX_NOACTIVATE and must not steal the
    // keyboard from whatever the user is typing in. Dismissal is driven by the global
    // mouse hook (outside click / wheel) instead of a blur event, which never fires for a
    // non-activating window.
    crate::dlogf!(
        "[app] reveal pos={:.0},{:.0} size={:.0}x{:.0} phys",
        x,
        y,
        w_px,
        h_px
    );
}

/// Live cursor position in physical screen pixels (Win32 GetCursorPos).
fn cursor_position() -> (i32, i32) {
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    extern "system" {
        fn GetCursorPos(p: *mut Point) -> i32;
    }
    let mut p = Point { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Convert a CSS-pixel size into the window's logical units.
///
/// Three different ratios exist in this stack and they are not equal:
///   logical -> physical = `scale_factor` (Tauri)      e.g. 1.5
///   physical -> CSS     = `devicePixelRatio` (webview) e.g. 1.8
///   logical -> CSS      = scale / dpr                  e.g. 0.833
/// Passing a CSS size straight to `LogicalSize` therefore yields only ~83% of the intended
/// viewport (which is exactly the compression that made the bar look broken). Converting
/// through devicePixelRatio is the physical/CSS relation that is actually consistent.
fn css_to_logical(css: f64, dpr: f64, scale: f64) -> f64 {
    let dpr = if dpr > 0.5 { dpr } else { scale };
    let scale = if scale > 0.1 { scale } else { 1.0 };
    css * dpr / scale
}

/// Monitor bounds in PHYSICAL pixels for the monitor nearest a physical point.
fn monitor_phys_at(app: &AppHandle, x: f64, y: f64) -> Option<(f64, f64, f64, f64)> {
    let m = app.monitor_from_point(x, y).ok().flatten()?;
    let s = m.size();
    let p = m.position();
    Some((p.x as f64, p.y as f64, s.width as f64, s.height as f64))
}


/// Window rectangle in physical pixels (Win32 GetWindowRect), matching the low-level
/// hook's coordinate space on DPI-scaled displays.
fn window_rect_px(win: &tauri::WebviewWindow) -> Option<(i32, i32, i32, i32)> {
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    }
    let hwnd = win.hwnd().ok()?.0 as isize;
    let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetWindowRect(hwnd, &mut r) } != 0 {
        Some((r.left, r.top, r.right, r.bottom))
    } else {
        None
    }
}

#[tauri::command]
fn toolbar_report_size(app: AppHandle, size: Value) {
    // The window must be EXACTLY the pill: the pill fills the window, so any extra width
    // would appear as a strip of bare material beside it. No slack is added.
    let width = size.get("width").and_then(|v| v.as_f64()).unwrap_or(360.0).ceil().max(120.0);
    let height = TOOLBAR_HEIGHT;
    let dpr = size.get("dpr").and_then(|v| v.as_f64()).unwrap_or(1.0).clamp(0.5, 4.0);
    // The webview may render at a different scale than the window reports; sizing must use
    // the webview's ratio, in physical pixels, or the CSS viewport comes out too small.
    // The webview's reported devicePixelRatio cannot be trusted for sizing (we measured
    // window 900px physical with innerWidth 747 and dpr 1.8 — mutually inconsistent). So
    // instead of converting units we CLOSE THE LOOP: ask for a size, observe the resulting
    // CSS viewport, and correct by the measured ratio. It converges in one step.
    // Physical = CSS * devicePixelRatio (see reveal_toolbar for the measurement that
    // established this). No calibration loop is needed.
    *app.state::<State>().toolbar_dpr.lock().unwrap() = dpr;
    // Only a measurement taken with an actual selection may reveal the window; the empty
    // initial render also reports.
    let has_payload = size.get("hasPayload").and_then(|v| v.as_bool()).unwrap_or(false);
    let pending = app.state::<State>().show_pending.load(Ordering::SeqCst);

    // Reveal path: this measurement describes a real selection, so size and show it.
    if has_payload && pending {
        if app.state::<State>().show_pending.swap(false, Ordering::SeqCst) {
            *app.state::<State>().toolbar_width.lock().unwrap() = width;
            reveal_toolbar(&app, width);
        }
        return;
    }

    // Otherwise only resize a toolbar that is already on screen (e.g. the action list
    // changed while visible). Resizing the hidden window here would shrink the very
    // window we are measuring in, compressing the layout and clipping the last action.
    // Dead-band: ignore sub-2px differences. Without it, a 1px rounding difference between
    // the window and the row re-triggers the ResizeObserver, which reports a 1px-larger
    // width, which resizes again — a runaway +1px growth loop.
    let visible = app
        .get_webview_window("toolbar")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if visible {
        let changed = {
            let state = app.state::<State>();
            let mut cur = state.toolbar_width.lock().unwrap();
            let changed = (*cur - width).abs() >= 2.0;
            if changed {
                *cur = width;
            }
            changed
        };
        if changed {
            if let Some(win) = app.get_webview_window("toolbar") {
                let scale = win.scale_factor().unwrap_or(1.0).max(0.1);
                let _ = win.set_size(tauri::LogicalSize::new(
                    css_to_logical(width, dpr, scale),
                    css_to_logical(height, dpr, scale),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tray
// ---------------------------------------------------------------------------

fn rebuild_tray(app: &AppHandle, s: &store::Settings) {
    let Some(tray) = app.tray_by_id("main") else { return };
    let enable_item = CheckMenuItem::with_id(app, "enabled", "启用 Wisp", true, s.enabled, None::<&str>).ok();
    let settings_item = MenuItem::with_id(app, "settings", "打开设置…", true, None::<&str>).ok();
    let show_item = MenuItem::with_id(app, "show", "显示悬浮条", true, None::<&str>).ok();
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).ok();
    let sep1 = PredefinedMenuItem::separator(app).ok();
    let sep2 = PredefinedMenuItem::separator(app).ok();

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = Vec::new();
    if let Some(i) = enable_item.as_ref() {
        items.push(i);
    }
    if let Some(i) = sep1.as_ref() {
        items.push(i);
    }
    if let Some(i) = settings_item.as_ref() {
        items.push(i);
    }
    if let Some(i) = show_item.as_ref() {
        items.push(i);
    }
    if let Some(i) = sep2.as_ref() {
        items.push(i);
    }
    if let Some(i) = quit_item.as_ref() {
        items.push(i);
    }
    if let Ok(menu) = Menu::with_items(app, &items) {
        let _ = tray.set_menu(Some(menu));
    }
    let _ = tray.set_tooltip(Some(if s.enabled { "Wisp" } else { "Wisp（已停用）" }));
}

fn on_tray_menu(app: &AppHandle, id: &str) {
    match id {
        "enabled" => {
            let s = store::load();
            let next = !s.enabled;
            let _ = settings_set(app.clone(), json!({ "enabled": next }));
        }
        "settings" => show_settings(app),
        "show" => {
            if let Some(sel) = selection::get_current_selection() {
                on_selection_event(app, sel);
            }
        }
        "quit" => {
            selection::cleanup();
            app.exit(0);
        }
        _ => {}
    }
}

fn on_selection_event(app: &AppHandle, payload: selection::SelectionPayload) {
    *app.state::<State>().last_selection.lock().unwrap() = Some(payload.clone());
    show_toolbar_at(app, &payload);
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Mark the process per-monitor DPI aware.
///
/// Must run before any window is created: otherwise Windows virtualises coordinates,
/// `scale_factor()` reports 1.0, and the hook's physical pixels no longer line up with
/// what the window system thinks a logical pixel is (the toolbar then renders at the
/// wrong size and position on scaled displays).
fn enable_dpi_awareness() {
    #[link(name = "user32")]
    extern "system" {
        fn SetProcessDpiAwarenessContext(value: isize) -> i32;
    }
    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 == -4
    unsafe {
        SetProcessDpiAwarenessContext(-4);
    }
}

pub fn run() {
    enable_dpi_awareness();
    tauri::Builder::default()
        // Must be the first plugin: a second launch hands off to this instance instead of
        // starting another process (which previously produced duplicate tray icons).
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .manage(State::default())
        .invoke_handler(tauri::generate_handler![
            settings_get,
            ui_prefs,
            settings_set,
            settings_reset,
            app_info,
            test_provider,
            open_settings,
            open_external,
            toolbar_hide,
            toolbar_get_state,
            toolbar_copy,
            toolbar_search,
            toolbar_report_size,
            run_action,
            action_get_context,
            action_run,
            action_stop,
            action_copy,
            action_close,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            selection::set_app(handle.clone());

            build_toolbar(&handle);
            build_action(&handle);
            rebuild_tray(&handle, &store::load());

            let h = handle.clone();
            handle.listen("selection", move |event| {
                if let Ok(p) = serde_json::from_str::<selection::SelectionPayload>(event.payload()) {
                    on_selection_event(&h, p);
                }
            });

            // Hide the toolbar on scroll only. Outside-clicks are handled by the toolbar
            // window losing focus (see on_window_event), which avoids any coordinate-space
            // mismatch between the low-level hook and the window system.
            let h2 = handle.clone();
            handle.listen("global-pointer", move |event| {
                let v: Value = serde_json::from_str(event.payload()).unwrap_or(json!({}));
                let action = v.get("action").and_then(|a| a.as_i64()).unwrap_or(-1);
                let px = v.get("x").and_then(|a| a.as_i64()).unwrap_or(0) as i32;
                let py = v.get("y").and_then(|a| a.as_i64()).unwrap_or(0) as i32;

                let Some(tb) = h2.get_webview_window("toolbar") else { return };
                if !tb.is_visible().unwrap_or(false) {
                    return;
                }
                // Wheel dismisses immediately. A button-down only dismisses when it lands
                // outside the bar — otherwise pressing an action would hide it before the
                // click registered. Both sides are compared in the hook's physical-pixel
                // space by asking Win32 for the window rect.
                if action == 3 {
                    let _ = tb.hide();
                    return;
                }
                if action == 0 {
                    if let Some((l, t, r, b)) = window_rect_px(&tb) {
                        let inside = px >= l && px <= r && py >= t && py <= b;
                        if !inside {
                            let _ = tb.hide();
                        }
                    }
                }
            });

            // Tray events.
            let h3 = handle.clone();
            if let Some(tray) = handle.tray_by_id("main") {
                tray.on_menu_event(move |_tray, ev| {
                    on_tray_menu(&h3, ev.id().as_ref());
                });
                let h4 = handle.clone();
                tray.on_tray_icon_event(move |_tray, ev| {
                    if let tauri::tray::TrayIconEvent::DoubleClick { .. } = ev {
                        show_settings(&h4);
                    }
                });
            }

            let s = store::load();
            if s.enabled {
                match selection::start(&hook_config(&s)) {
                    Ok(_) => crate::dlogf!("[app] selection hook started"),
                    Err(e) => crate::dlogf!("[app] hook start failed: {e}"),
                }
            }

            // `--settings` opens settings on launch (also handy for scripting). On a
            // first run, when no model provider has been configured yet, open it
            // automatically so the app is usable without hunting through the tray.
            let no_provider = s.providers.iter().all(|p| p.api_base.trim().is_empty() || p.model.trim().is_empty());
            crate::dlogf!("[app] setup done; providers={} no_provider={no_provider}", s.providers.len());
            if std::env::args().any(|a| a == "--settings") || no_provider {
                show_settings(&handle);
            }

            // `--preview` shows the toolbar with sample text at the cursor, without needing
            // a real selection. Useful for inspecting the UI (and for automated visual
            // checks) without taking over the user's mouse.
            if std::env::args().any(|a| a == "--preview") {
                PREVIEW.store(true, Ordering::SeqCst);
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    // Anchor in the middle of the primary monitor: the cursor could be
                    // anywhere, including a screen edge where the bar would be clamped.
                    let (cx, cy) = h
                        .primary_monitor()
                        .ok()
                        .flatten()
                        .map(|m| {
                            let sc = m.scale_factor().max(0.1);
                            (
                                (m.position().x as f64 + m.size().width as f64 / 2.0) / sc,
                                (m.position().y as f64 + m.size().height as f64 * 0.42) / sc,
                            )
                        })
                        .unwrap_or((300.0, 300.0));
                    // Hand the anchor through in physical units, matching the real path.
                    let sc = h
                        .primary_monitor()
                        .ok()
                        .flatten()
                        .map(|m| m.scale_factor().max(0.1))
                        .unwrap_or(1.0);
                    let (px, py) = ((cx * sc) as i32, (cy * sc) as i32);
                    on_selection_event(
                        &h,
                        selection::SelectionPayload {
                            text: "preview".into(),
                            program_name: "preview".into(),
                            pos_level: 3,
                            ref_point: selection::SPoint { x: px, y: py },
                            mouse_pos_end: selection::SPoint { x: px, y: py },
                            start_top: selection::SPoint { x: px, y: py },
                            end_bottom: selection::SPoint { x: px, y: py },
                        },
                    );
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the settings window hides it; the app stays in the tray.
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "settings" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            // The floating toolbar dismisses itself when it loses focus (an outside
            // click), matching Cherry Studio's hide-on-blur toolbar behaviour.
            // The toolbar is non-activating, so it receives no blur; dismissal is handled
            // by the global mouse hook. Nothing to do here.
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error building tauri app")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                selection::cleanup();
            }
            let _ = app;
        });
}
