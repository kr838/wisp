//! Wisp — a ~2 MB Windows selection assistant.
//!
//! The original C++ selection engine is reused through a C ABI DLL (`selection.rs`), and the
//! UI is plain HTML/CSS/JS rendered by the system WebView2 (`ui/`). This module owns the
//! windows, the tray, the IPC surface and the app lifecycle.
//!
//! # Window model
//!
//! | window        | role                                   | topmost | size          |
//! |---------------|----------------------------------------|---------|---------------|
//! | `toolbar`     | floating action bar near the selection | yes     | content-sized |
//! | `action`      | streaming result panel (one per action)| yes     | resizable     |
//! | `action-<n>`  | additional result panels (multi-open)  | yes     | resizable     |
//! | `settings`    | configuration (native frame)           | no      | fixed         |
//!
//! Only the floating windows are always-on-top; the settings window is a normal window so
//! the user can switch away from it without minimising. Result panels are created on demand
//! and destroyed when closed, so any number of results can be open at once — the previous
//! single fixed `action` window was reused and therefore replaced whatever the user was
//! reading.
//!
//! # Sizing / DPI note
//!
//! Three ratios exist and are NOT equal: window logical -> physical (`scale_factor`), physical
//! -> CSS (`devicePixelRatio`), and therefore logical -> CSS. Sizes are derived as
//! `physical = CSS * devicePixelRatio`; see `css_to_logical` and `reveal_toolbar`. Sizes are
//! never read back from the window right after setting them, because the resize applies
//! asynchronously and the stale value misplaces the toolbar.

mod ai;
mod chat;
mod panels;
mod selection;
pub mod sse;
mod store;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_opener::OpenerExt;

/// Everything one open result panel needs to know.
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
    /// Window label of the panel this action runs in.
    #[serde(rename = "windowLabel")]
    window_label: String,
}

/// Everything a panel needs beyond its conversation: which action produced it, and the
/// provider/model snapshot taken when it opened.
struct Panel {
    pending: PendingAction,
}

impl Panel {
    fn provider(&self) -> Option<store::Provider> {
        let p = self.pending.provider.clone()?;
        serde_json::from_value::<store::Provider>(p).ok()
    }
    fn level(&self) -> String {
        self.pending.thinking_level.clone()
    }
}

/// All open panels. The conversation state machine lives in `panels` (pure logic, unit-tested);
/// this map only holds the per-window provider snapshot and action metadata.
struct PanelsState {
    meta: HashMap<String, Panel>,
    conv: panels::Panels,
}

impl Default for PanelsState {
    fn default() -> Self {
        PanelsState { meta: HashMap::new(), conv: panels::Panels::default() }
    }
}

struct State {
    last_selection: Mutex<Option<selection::SelectionPayload>>,
    panels: Mutex<PanelsState>,
    /// Toolbar width reported by the renderer, in CSS pixels.
    toolbar_width: Mutex<f64>,
    /// The webview's own device pixel ratio (from the renderer). It can differ from the
    /// window's `scale_factor`, and window sizing must use this one.
    toolbar_dpr: Mutex<f64>,
    /// Anchor (center_x, top_y, bottom_y) of the current toolbar placement, in physical px.
    toolbar_anchor: Mutex<Option<(f64, f64, f64)>>,
    /// True while the toolbar content is being rendered off-screen; it is shown by
    /// `toolbar_report_size` once the real size is known (avoids a visible size jump).
    show_pending: AtomicBool,
    /// Serialises panel creation so two rapid action clicks cannot both create a window.
    panel_seq: AtomicU64,
    /// The tray's "启用 Wisp" check item, kept so its state can be flipped in place instead
    /// of rebuilding the whole native menu (which leaked handles on every save).
    tray_enabled_item: Mutex<Option<tauri::menu::CheckMenuItem<tauri::Wry>>>,
    /// Result-window geometry, remembered across panels so a resized panel reopens at the
    /// size the user chose instead of the hard-coded default.
    panel_size: Mutex<Option<(f64, f64)>>,
}

impl Default for State {
    fn default() -> Self {
        State {
            last_selection: Mutex::new(None),
            panels: Mutex::new(PanelsState::default()),
            toolbar_width: Mutex::new(360.0),
            toolbar_dpr: Mutex::new(1.0),
            toolbar_anchor: Mutex::new(None),
            show_pending: AtomicBool::new(false),
            panel_seq: AtomicU64::new(0),
            tray_enabled_item: Mutex::new(None),
            panel_size: Mutex::new(None),
        }
    }
}

/// Preview mode (`--preview`): keep the toolbar visible instead of hiding it, so
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

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "%20".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Commands: settings
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
    update_tray(&app, &next);
    // Material / tint changes are native, so they must be pushed to the live windows.
    apply_material_all(&app);
    let _ = app.emit("settings-changed", settings_json(&next));
    settings_json(&next)
}

#[tauri::command]
fn settings_reset(app: AppHandle) -> Value {
    let s = store::reset();
    let _ = selection::start(&hook_config(&s));
    update_tray(&app, &s);
    let _ = app.emit("settings-changed", settings_json(&s));
    settings_json(&s)
}

#[tauri::command]
async fn test_provider(provider: store::Provider) -> Value {
    let (ok, msg) = ai::test_provider(&provider).await;
    json!({ "ok": ok, "message": msg })
}

/// Open the settings window from the toolbar.
///
/// Async for the same reason as `run_action`: `show_settings` may create a window, and window
/// creation cannot complete while the main thread is blocked inside the IPC callback.
#[tauri::command]
async fn open_settings(app: AppHandle) {
    show_settings(&app);
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) {
    let _ = app.opener().open_url(url, None::<&str>);
}

// ---------------------------------------------------------------------------
// Commands: toolbar
// ---------------------------------------------------------------------------

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

/// Copy the selection to the clipboard (the toolbar's local "复制" action).
#[tauri::command]
fn toolbar_copy(app: AppHandle, text: String) {
    selection::write_clipboard(&text);
    hide_toolbar(&app);
}

/// Pick the provider for an action and report whether AI can run at all.
///
/// The toolbar calls this before showing an error: a missing provider should open the
/// settings window instead of silently doing nothing when an AI action is clicked.
// ---------------------------------------------------------------------------
// Commands: result panels
// ---------------------------------------------------------------------------

/// Create a new result panel and return its label.
///
/// Each click on an action opens a NEW window (`action-1`, `action-2`, …) instead of reusing
/// one fixed window, so previous answers stay on screen. Windows are freed when closed (see
/// the `Destroyed` handler), which bounds the number of live webviews.
fn new_panel(app: &AppHandle, pending: PendingAction) -> Result<String, String> {
    let n = app.state::<State>().panel_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let label = format!("action-{n}");
    let mut p = pending.clone();
    p.window_label = label.clone();

    {
        let st = app.state::<State>();
        let mut panels = st.panels.lock().unwrap();
        panels.meta.insert(label.clone(), Panel { pending: p.clone() });
        panels.conv.open(&label, &ai::system_prompt(&pending.system_prompt), &pending.text);
    }

    build_action(app, &label).map_err(|e| {
        // Do not leave a panel entry behind for a window that was never created: it would
        // keep a provider snapshot and a conversation alive for the rest of the session.
        if let Some(st) = app.try_state::<State>() {
            let mut panels = st.panels.lock().unwrap();
            panels.meta.remove(&label);
            panels.conv.remove(&label);
        }
        e
    })?;
    if let Some(w) = app.get_webview_window(&label) {
        position_action(app, &w);
        let _ = w.show();
        let _ = w.set_focus();
        // The renderer asks for its context on load; the event covers a panel that is
        // already loaded (a second action in the same window label never happens, but the
        // emit also gives the page a chance to start without a polling loop).
        let _ = app.emit_to(&label, "action-start", p);
    }
    Ok(label)
}

/// Run an action and open its result panel.
///
/// This MUST stay `async`. A synchronous Tauri command executes inside the webview's IPC
/// callback, which runs on the main thread. Creating a `WebviewWindow` from there deadlocks:
/// window creation dispatches a task to the main thread and waits for it, but the main thread
/// is still inside the IPC callback waiting for the command to return — so neither side can
/// proceed. The visible symptom is exactly what the toolbar shows: the bar dims (its `busy`
/// state) and no result window ever appears.
///
/// As an `async` command the body runs on the async runtime, so `build()` dispatches to a
/// free main thread and returns normally. `--selftest-panels` always worked because it calls
/// the same `new_panel` from a spawned task.
#[tauri::command]
async fn run_action(app: AppHandle, action_id: String) -> Value {
    let s = store::load();
    let Some(action) = s.actions.iter().find(|a| a.id == action_id).cloned() else {
        return json!({ "ok": false, "error": "动作不存在" });
    };
    let sel = {
        let st = app.state::<State>();
        let g = st.last_selection.lock().unwrap();
        g.clone()
    };
    let Some(sel) = sel else {
        crate::dlogf!("[app] run_action {action_id}: no selection");
        return json!({ "ok": false, "error": "没有选中文本" });
    };

    // Local actions never call a model, so they must not require one. They are recognised by
    // their fixed id — there is no action-type field any more.
    if action.id == store::LOCAL_COPY {
        selection::write_clipboard(&sel.text);
        hide_toolbar(&app);
        return json!({ "ok": true, "local": true });
    }
    if action.id == store::LOCAL_SEARCH {
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
        action: json!({ "id": action.id, "name": action.name, "color": action.color }),
        text: sel.text.clone(),
        program_name: sel.program_name.clone(),
        system_prompt: ai::system_prompt(&action.prompt),
        model_name: provider.as_ref().map(|p| p.model.clone()).unwrap_or_default(),
        provider: provider.as_ref().map(|p| serde_json::to_value(p).unwrap_or(json!({}))),
        thinking_level: level,
        thinking_supported: supported,
        window_label: String::new(),
    };

    hide_toolbar(&app);
    match new_panel(&app, pending) {
        Ok(label) => {
            crate::dlogf!("[app] run_action {} -> panel {label}", action_id);
            json!({ "ok": true, "windowLabel": label })
        }
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/// Return the context for the calling panel.
#[tauri::command]
fn action_get_context(window: WebviewWindow, app: AppHandle) -> Value {
    let label = window.label().to_string();
    let st = app.state::<State>();
    let panels = st.panels.lock().unwrap();
    match panels.meta.get(&label) {
        Some(p) => serde_json::to_value(&p.pending).unwrap_or(json!(null)),
        None => json!(null),
    }
}

#[tauri::command]
fn action_close(window: WebviewWindow) {
    // `close` runs the normal close path; the `Destroyed` hook then drops the panel state.
    let _ = window.close();
}

#[tauri::command]
fn action_stop(window: WebviewWindow, app: AppHandle) {
    let label = window.label().to_string();
    {
        let st = app.state::<State>();
        let mut panels = st.panels.lock().unwrap();
        panels.conv.stop(&label);
    }
    let _ = app.emit_to(&label, "action-aborted", ());
}

#[tauri::command]
fn action_run(window: WebviewWindow, app: AppHandle) {
    let label = window.label().to_string();
    let (provider, level, generation, messages) = {
        let st = app.state::<State>();
        let mut panels = st.panels.lock().unwrap();
        let Some(meta) = panels.meta.get(&label) else {
            drop(panels);
            let _ = app.emit_to(&label, "action-error", "窗口状态丢失，请关闭后重试");
            return;
        };
        let Some(prov) = meta.provider() else {
            drop(panels);
            let _ = app.emit_to(&label, "action-error", "未配置 AI 模型，请先在设置中添加模型提供商");
            return;
        };
        let level = meta.level();
        // `begin` replays the first turn from the initial messages, so pressing 重试 after a
        // failure sends the original request once instead of appending it again.
        match panels.conv.begin(&label) {
            Some((generation, messages)) => (prov, level, generation, messages),
            None => {
                drop(panels);
                let _ = app.emit_to(&label, "action-error", "窗口状态丢失，请关闭后重试");
                return;
            }
        }
    };
    stream_panel(&app, &label, provider, level, generation, messages);
}

/// Stream the panel's current conversation.
///
/// Shared by the initial action and by 追问, so both paths emit exactly the same events
/// (`action-chunk` / `action-reasoning` / `action-done` / `action-error`). The assistant reply
/// is appended to the conversation so the next follow-up has full context.
///
/// Every request carries a generation number. If the user stops the turn or asks a new
/// question, the generation advances and the stale stream's events are dropped — that is what
/// prevents a stopped answer from reappearing when the HTTP body finally arrives.
fn stream_panel(
    app: &AppHandle,
    label: &str,
    provider: store::Provider,
    level: String,
    generation: u64,
    messages: Vec<ai::ChatMessage>,
) {
    crate::dlogf!("[app] stream {label} model={} level={} turns={}", provider.model, level, messages.len());

    let app2 = app.clone();
    let label_owned = label.to_string();
    tauri::async_runtime::spawn(async move {
        let app_chunk = app2.clone();
        let label_chunk = label_owned.clone();
        let app_reason = app2.clone();
        let label_reason = label_owned.clone();
        let res = ai::chat_stream(
            &provider,
            &messages,
            &level,
            move |t| {
                let _ = app_chunk.emit_to(&label_chunk, "action-chunk", t.to_string());
            },
            move |t| {
                let _ = app_reason.emit_to(&label_reason, "action-reasoning", t.to_string());
            },
        )
        .await;

        let st = app2.state::<State>();
        let mut panels = st.panels.lock().unwrap();
        match res {
            Ok(out) if !out.text.trim().is_empty() => {
                crate::dlogf!("[app] stream done {label_owned} textLen={}", out.text.chars().count());
                // `finish` refuses a superseded generation, so a stopped turn cannot append
                // its answer after the user moved on.
                if panels.conv.finish(&label_owned, generation, &out.text) {
                    drop(panels);
                    let _ = app2.emit_to(&label_owned, "action-done", out.text);
                }
            }
            Ok(_) => {
                panels.conv.fail(&label_owned, generation);
                drop(panels);
                let _ = app2.emit_to(&label_owned, "action-error", "模型没有返回内容");
            }
            Err(e) => {
                crate::dlogf!("[app] stream error {label_owned}: {e}");
                panels.conv.fail(&label_owned, generation);
                drop(panels);
                let _ = app2.emit_to(&label_owned, "action-error", e);
            }
        }
    });
}

/// Ask a follow-up question in the calling panel.
#[tauri::command]
fn action_ask(window: WebviewWindow, app: AppHandle, text: String) -> Value {
    let label = window.label().to_string();
    let text = text.trim().to_string();
    if text.is_empty() {
        return json!({ "ok": false, "error": "内容为空" });
    }

    let (provider, level, generation, messages) = {
        let st = app.state::<State>();
        let mut panels = st.panels.lock().unwrap();
        let Some(meta) = panels.meta.get(&label) else {
            return json!({ "ok": false, "error": "窗口状态丢失，请关闭后重试" });
        };
        let Some(prov) = meta.provider() else {
            return json!({ "ok": false, "error": "未配置 AI 模型" });
        };
        let level = meta.level();
        match panels.conv.ask(&label, &text) {
            // `ask` appends the question and starts the turn; `fail` pops it again on error.
            Some((generation, messages)) => (prov, level, generation, messages),
            None => return json!({ "ok": false, "error": "窗口状态丢失，请关闭后重试" }),
        }
    };

    let _ = app.emit_to(&label, "action-turn", json!({ "user": text }));
    stream_panel(&app, &label, provider, level, generation, messages);
    json!({ "ok": true })
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// Fixed toolbar height, matching Cherry Studio: the pill is 36px.
const TOOLBAR_HEIGHT: f64 = 36.0;

/// Width the hidden toolbar is stretched to while it measures its content. It must be
/// wider than the tallest expected row, but is capped to the screen width so the window
/// never extends off-screen (a clipped window makes the renderer under-report).
const MEASURE_WIDTH: f64 = 900.0;
/// Lower bound when reusing the previous width for the measuring pass.
const MEASURE_WIDTH_MIN: f64 = 360.0;

fn build_toolbar(app: &AppHandle) {
    if app.get_webview_window("toolbar").is_some() {
        return;
    }
    // focusable(false) sets WS_EX_NOACTIVATE: the toolbar still receives mouse clicks but
    // never takes keyboard focus, so the user keeps typing / using Ctrl+C in their own app.
    let built = WebviewWindowBuilder::new(app, "toolbar", WebviewUrl::App("toolbar/index.html".into()))
        .title("Wisp")
        .inner_size(MEASURE_WIDTH_MIN, TOOLBAR_HEIGHT)
        .position(-10000.0, -10000.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        // `focused(false)` only avoids focusing the window when it is created; it does NOT
        // prevent it from being activated later. `focusable(false)` is what sets
        // WS_EX_NOACTIVATE, so clicking a chip cannot steal the keyboard from the app the
        // text was selected in (that made typing and Ctrl+C stop working).
        .focused(false)
        .focusable(false)
        // No OS shadow: it brings an invisible resize frame that eats into the client
        // area and would clip the last chip. The CSS hairline provides the edge instead.
        .shadow(false)
        .visible(false)
        .build();
    if let Ok(w) = built {
        apply_material(&w, &store::load());
    }
}

/// Give a window the configured backdrop material with rounded corners.
///
/// CSS `backdrop-filter` cannot blur the desktop behind a separate window, so the blur has
/// to come from the OS. Rounding the window itself (rather than drawing a rounded pill
/// inside a rectangular window) is what prevents the "rounded card floating in a translucent
/// rectangle" look: the acrylic region then *is* the surface.
fn apply_material(w: &WebviewWindow, s: &store::Settings) {
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
    for (_, w) in app.webview_windows() {
        let label = w.label();
        if label == "toolbar" || label.starts_with("action") {
            apply_material(&w, &s);
        }
    }
}

/// Ask DWM to round the window corners (Windows 11). Ignored on Windows 10.
fn round_window_corners(w: &WebviewWindow) {
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

fn build_action(app: &AppHandle, label: &str) -> Result<(), String> {
    // Reuse the size the user last chose, so resizing a panel has an effect on the next one.
    let (w, h) = app.state::<State>().panel_size.lock().unwrap().unwrap_or((560.0, 460.0));
    // One surface only: the window itself is the frosted panel (acrylic/mica + rounded
    // corners). The webview paints no background and no inset card.
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::App("action/index.html".into()))
        .title("Wisp")
        .inner_size(w, h)
        .min_inner_size(360.0, 200.0)
        .decorations(false)
        .transparent(true)
        // NOT always-on-top: several result panels are meant to stay open side by side, and a
        // topmost panel covers the others (and the user's work) until it is closed. The
        // toolbar stays topmost because it must appear over the app the text came from.
        .always_on_top(false)
        .skip_taskbar(false)
        .resizable(true)
        // The OS shadow draws around the whole transparent rectangle, not the rounded
        // panel, which is what produced the "big transparent frame" look.
        .shadow(false)
        .visible(false)
        .build()
        .map_err(|e| format!("创建结果窗口失败: {e}"))?;

    apply_material(&built, &store::load());
    Ok(())
}

/// Show the settings window.
///
/// The floating windows are hidden first so the settings window is not buried; settings
/// itself stays a normal window (making it topmost kept it above every other app).
fn show_settings(app: &AppHandle) {
    for label in ["toolbar"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    }
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.set_always_on_top(false);
        let _ = w.show();
        let _ = w.set_focus();
        crate::dlogf!("[app] settings window re-shown");
        return;
    }
    match WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("Wisp · 设置")
        .inner_size(880.0, 640.0)
        .min_inner_size(720.0, 520.0)
        .center()
        // Deliberately NOT always-on-top: settings is a normal window the user switches to
        // and away from. Only the floating toolbar and the result panels belong on top.
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

/// Place a result panel: under the toolbar when "follow toolbar" is on, else centred.
///
/// Open panels cascade by a small offset, so a second result is visibly a second window
/// instead of landing exactly on the first one.
fn position_action(app: &AppHandle, w: &WebviewWindow) {
    let existing = app
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("action-"))
        .count();
    // Count includes the panel being placed, so the first one gets offset 0.
    let step = (existing.saturating_sub(1) % 6) as f64 * 28.0;

    let s = store::load();
    if s.follow_toolbar {
        if let Some(tb) = app.get_webview_window("toolbar") {
            // Use logical (DIP) coordinates to stay consistent with the toolbar window.
            if let (Ok(tbpos), Ok(tbsize)) = (tb.outer_position(), tb.outer_size()) {
                let scale = tb.scale_factor().unwrap_or(1.0).max(0.1);
                let (x, y) = (tbpos.x as f64 / scale, tbpos.y as f64 / scale);
                let h = tbsize.height as f64 / scale;
                let _ = w.set_position(tauri::LogicalPosition::new(x + step, y + h + 6.0 + step));
                return;
            }
        }
    }
    if let Ok(size) = w.outer_size() {
        if let Some(mon) = w.current_monitor().ok().flatten() {
            let scale = mon.scale_factor().max(0.1);
            let mw = mon.size().width as f64 / scale;
            let mh = mon.size().height as f64 / scale;
            let ww = size.width as f64 / scale;
            let wh = size.height as f64 / scale;
            let x = ((mw - ww) / 2.0 + step).max(0.0);
            let y = ((mh - wh) / 2.0 + step).max(0.0);
            let mx = mon.position().x as f64 / scale;
            let my = mon.position().y as f64 / scale;
            let _ = w.set_position(tauri::LogicalPosition::new(mx + x, my + y));
            return;
        }
    }
    let _ = w.center();
}

/// Show the floating toolbar for a selection.
///
/// The window is *not* shown here: its content is rendered while hidden and the real
/// size comes back through `toolbar_report_size`, which then positions and reveals it.
fn show_toolbar_at(app: &AppHandle, payload: &selection::SelectionPayload) {
    if app.get_webview_window("toolbar").is_none() {
        build_toolbar(app);
    }
    let Some(w) = app.get_webview_window("toolbar") else { return };

    // The low-level hook reports *physical* pixels. The anchor is stored raw here and
    // converted to logical pixels (DIP) in reveal_toolbar, where the window's scale
    // factor is known to be settled.
    // The clipboard fallback (Win32 edit controls and similar) returns text without
    // coordinates, so pos_level is 0 and every point is (0,0). Fall back to the live
    // cursor position.
    let no_position = payload.pos_level <= 0
        || (payload.ref_point.x == 0 && payload.ref_point.y == 0 && payload.mouse_pos_end.x == 0);

    let (anchor_x, below_y, anchor_bottom) = if no_position {
        let (px, py) = cursor_position();
        (px as f64, py as f64 + 10.0, py as f64)
    } else {
        match payload.pos_level {
            3 | 4 => (
                payload.end_bottom.x as f64,
                payload.end_bottom.y as f64 + 10.0,
                payload.end_bottom.y as f64,
            ),
            _ => (
                payload.mouse_pos_end.x as f64,
                payload.mouse_pos_end.y as f64 + 18.0,
                payload.mouse_pos_end.y as f64,
            ),
        }
    };

    *app.state::<State>().toolbar_anchor.lock().unwrap() = Some((anchor_x, below_y, anchor_bottom));
    app.state::<State>().show_pending.store(true, Ordering::SeqCst);

    // Pre-widen the (still hidden) window before the renderer measures: a window narrower
    // than the content compresses the row, so the reported width would be too small and the
    // last action would be clipped.
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
    let _ = w.set_size(tauri::LogicalSize::new(
        css_to_logical(measure_w, mdpr, mscale),
        css_to_logical(TOOLBAR_HEIGHT, mdpr, mscale),
    ));
    let _ = app.emit_to("toolbar", "toolbar-payload", payload.clone());

    // Fallback: if the renderer never reports a size, reveal the toolbar anyway so a
    // front-end error cannot make selection appear to do nothing.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if app2.state::<State>().show_pending.swap(false, Ordering::SeqCst) {
            let width = *app2.state::<State>().toolbar_width.lock().unwrap();
            reveal_toolbar(&app2, width);
        }
    });
}

/// Position and show the toolbar using a known content width (CSS pixels).
fn reveal_toolbar(app: &AppHandle, width: f64) {
    let Some(w) = app.get_webview_window("toolbar") else { return };
    let anchor = *app.state::<State>().toolbar_anchor.lock().unwrap();
    let Some((anchor_x_px, below_y_px, anchor_bottom_px)) = anchor else { return };

    // Sizing: physical = CSS * devicePixelRatio. Read back from the window is unsafe (the
    // resize is asynchronous), so both size and position are derived from that formula.
    let dpr = {
        let d = *app.state::<State>().toolbar_dpr.lock().unwrap();
        if d > 0.5 { d } else { w.scale_factor().unwrap_or(1.0).max(0.5) }
    };
    let css_w = width.max(120.0);
    let w_px = (css_w * dpr).round();
    let h_px = (TOOLBAR_HEIGHT * dpr).round();

    let scale = w.scale_factor().unwrap_or(1.0).max(0.1);
    let _ = w.set_size(tauri::LogicalSize::new(
        css_to_logical(css_w, dpr, scale),
        css_to_logical(TOOLBAR_HEIGHT, dpr, scale),
    ));

    let mut x = anchor_x_px - w_px / 2.0;
    let mut y = below_y_px;
    // Clamp to the monitor containing the anchor, in physical pixels.
    if let Some((mx, my, mw, mh)) = monitor_phys_at(app, anchor_x_px, below_y_px) {
        let margin = 8.0;
        let max_x = mx + mw - w_px - margin;
        let max_y = my + mh - h_px - margin;
        if x > max_x {
            x = max_x;
        }
        if y > max_y {
            // Not enough room below: place it above the anchor.
            y = (anchor_bottom_px - h_px - margin).max(my + margin);
        }
        x = x.max(mx + margin);
        y = y.max(my + margin);
    }
    let _ = w.set_position(tauri::PhysicalPosition::new(x.round() as i32, y.round() as i32));
    let _ = w.show();
    // Deliberately no set_focus(): the window is WS_EX_NOACTIVATE and must not steal the
    // keyboard from whatever the user is typing in.
    crate::dlogf!("[app] reveal pos={:.0},{:.0} size={:.0}x{:.0} phys", x, y, w_px, h_px);
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
fn window_rect_px(win: &WebviewWindow) -> Option<(i32, i32, i32, i32)> {
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
    // Sizing must use the webview's ratio, in physical pixels, or the CSS viewport comes
    // out too small. Physical = CSS * devicePixelRatio (see reveal_toolbar).
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
    // Dead-band: ignore sub-2px differences, otherwise a 1px rounding difference between
    // the window and the row re-triggers the ResizeObserver forever.
    let visible = app
        .get_webview_window("toolbar")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if visible {
        let changed = {
            let st = app.state::<State>();
            let mut cur = st.toolbar_width.lock().unwrap();
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

/// Build the tray menu once and remember the check item.
///
/// Updating the menu later only flips that item's check state — rebuilding the whole menu on
/// every settings change leaked native menu handles and dispatched work to the main thread
/// while streams were running.
fn build_tray_menu(app: &AppHandle, s: &store::Settings) -> Option<Menu<tauri::Wry>> {
    let enable_item = CheckMenuItem::with_id(app, "enabled", "启用 Wisp", true, s.enabled, None::<&str>).ok()?;
    let settings_item = MenuItem::with_id(app, "settings", "打开设置…", true, None::<&str>).ok()?;
    let show_item = MenuItem::with_id(app, "show", "显示悬浮条", true, None::<&str>).ok()?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).ok()?;
    let sep1 = PredefinedMenuItem::separator(app).ok()?;
    let sep2 = PredefinedMenuItem::separator(app).ok()?;
    let menu = Menu::with_items(
        app,
        &[&enable_item, &sep1, &settings_item, &show_item, &sep2, &quit_item],
    )
    .ok()?;
    *app.state::<State>().tray_enabled_item.lock().unwrap() = Some(enable_item);
    Some(menu)
}

fn update_tray(app: &AppHandle, s: &store::Settings) {
    let Some(tray) = app.tray_by_id("main") else { return };
    let existing = app.state::<State>().tray_enabled_item.lock().unwrap().clone();
    match existing {
        Some(item) => {
            let _ = item.set_checked(s.enabled);
        }
        None => {
            if let Some(menu) = build_tray_menu(app, s) {
                let _ = tray.set_menu(Some(menu));
            }
        }
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
            // Asynchronous on purpose: the synchronous variant ran UIAutomation on the main
            // thread and froze the whole app whenever the foreground application was busy.
            if !selection::request_selection(true) {
                crate::dlogf!("[app] tray 'show': no selection request queued");
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
// Ctrl-hold trigger
// ---------------------------------------------------------------------------

/// Hold-Ctrl trigger state.
///
/// In `ctrlkey` mode the hook does not detect mouse gestures at all; instead the toolbar is
/// shown when Ctrl is held down for a moment while text is selected. The previous build only
/// set the hook's passive flag and never listened for the key, so this trigger mode silently
/// did nothing.
struct CtrlTrigger {
    /// When the current Ctrl press started, or None when Ctrl is not down.
    since: Option<Instant>,
    /// Set once this press has already fired, so holding it does not repeat.
    fired: bool,
}

static CTRL_TRIGGER: Mutex<CtrlTrigger> = Mutex::new(CtrlTrigger { since: None, fired: false });

/// How long Ctrl has to be held before the toolbar appears.
const CTRL_HOLD_MS: u128 = 350;

fn on_global_key(_app: &AppHandle, v: &Value) {
    if store::load().trigger_mode != "ctrlkey" {
        return;
    }
    let key = v.get("uniKey").and_then(|k| k.as_str()).unwrap_or("");
    let action = v.get("action").and_then(|a| a.as_i64()).unwrap_or(0);
    // Ignore synthetic key events: the clipboard fallback injects Ctrl+C/Ctrl+V, which would
    // otherwise re-trigger the toolbar in a loop. LLKHF_INJECTED == 0x10.
    let injected = v.get("flags").and_then(|f| f.as_i64()).unwrap_or(0) & 0x10 != 0;
    if injected {
        return;
    }

    let mut t = CTRL_TRIGGER.lock().unwrap();
    if key != "Control" {
        // Any other key cancels a pending hold: the user is typing or using Ctrl+C.
        if action == 0 {
            t.since = None;
            t.fired = false;
        }
        return;
    }
    if action == 0 {
        // Key-down only starts the clock; the watcher below fires once the hold is long
        // enough. Firing here would open the bar for every ordinary Ctrl press.
        if t.since.is_none() {
            t.since = Some(Instant::now());
            t.fired = false;
        }
    } else {
        // Key-up ends the hold. The watcher is the only place that fires, otherwise a long
        // hold followed by release would request the selection twice.
        t.since = None;
        t.fired = false;
    }
}

/// Watch for Ctrl being held down while a selection exists.
///
/// The trigger is a *hold*, so it cannot be decided in the key-down handler. This runs on the
/// app's async runtime, samples the hold state, and requests the selection exactly once per
/// press.
fn spawn_ctrl_hold_watcher() {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(60)).await;
            // Only touch the settings store when a hold is actually in progress.
            let due = {
                let mut t = CTRL_TRIGGER.lock().unwrap();
                match t.since {
                    Some(start) if !t.fired => {
                        if start.elapsed() >= Duration::from_millis(CTRL_HOLD_MS as u64) {
                            t.fired = true;
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            };
            if due && store::load().trigger_mode == "ctrlkey" {
                if !selection::request_selection(true) {
                    crate::dlogf!("[app] ctrl trigger: request dropped");
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    enable_dpi_awareness();
    tauri::Builder::default()
        // Must be the first plugin: a second launch hands off to this instance instead of
        // starting another process (which previously produced duplicate tray icons).
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Relaunching with --settings (or a plain relaunch) should surface the settings
            // window; the argv is available so a future deep link can be routed.
            if argv.iter().any(|a| a == "--settings") || argv.len() <= 1 {
                show_settings(app);
            }
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
            toolbar_report_size,
            run_action,
            action_get_context,
            action_run,
            action_ask,
            action_stop,
            action_close,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            selection::set_app(handle.clone());

            build_toolbar(&handle);
            update_tray(&handle, &store::load());

            let h = handle.clone();
            handle.listen("selection", move |event| {
                if let Ok(p) = serde_json::from_str::<selection::SelectionPayload>(event.payload()) {
                    on_selection_event(&h, p);
                }
            });

            // Ctrl-hold trigger.
            let hk = handle.clone();
            handle.listen("global-key", move |event| {
                if let Ok(v) = serde_json::from_str::<Value>(event.payload()) {
                    on_global_key(&hk, &v);
                }
            });
            spawn_ctrl_hold_watcher();

            // Hide the toolbar on scroll and on clicks outside it. Both sides are compared in
            // the hook's physical-pixel space by asking Win32 for the window rect.
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
                // click registered.
                if action == 3 {
                    let _ = tb.hide();
                    return;
                }
                if action == 0 {
                    if let Some((l, t, r, b)) = window_rect_px(&tb) {
                        let inside = px >= l && px <= r && py >= t && py <= b;
                        if !inside && !PREVIEW.load(Ordering::SeqCst) {
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

            // `--settings` opens settings on launch (also handy for scripting). On a first
            // run, when no model provider has been configured yet, open it automatically so
            // the app is usable without hunting through the tray.
            let no_provider = s
                .providers
                .iter()
                .all(|p| p.api_base.trim().is_empty() || p.model.trim().is_empty());
            crate::dlogf!("[app] setup done; providers={} no_provider={no_provider}", s.providers.len());
            if std::env::args().any(|a| a == "--settings") || no_provider {
                show_settings(&handle);
            }

            // `--selftest-chat <url>` runs a two-turn conversation against an endpoint and
            // logs the result. It exercises the real request path (message list, streaming
            // parse, follow-up context) inside the app binary, which links the native engine
            // correctly — integration tests cannot, they fail at load time.
            let args: Vec<String> = std::env::args().collect();
            if let Some(i) = args.iter().position(|a| a == "--selftest-chat") {
                let base = args.get(i + 1).cloned().unwrap_or_else(|| "http://127.0.0.1:8799/v1".into());
                let provider = store::Provider {
                    id: "selftest".into(),
                    name: "selftest".into(),
                    api_base: base,
                    api_key: "test".into(),
                    model: "mock-model".into(),
                    temperature: Some(0.0),
                    max_tokens: None,
                    thinking: "auto".into(),
                    thinking_level: None,
                    headers: vec![],
                };
                tauri::async_runtime::spawn(async move {
                    use ai::ChatMessage;
                    let mut conv = vec![ChatMessage::system("sys"), ChatMessage::user("hello")];
                    let r1 = ai::chat_stream(&provider, &conv, "off", |_| {}, |_| {}).await;
                    match r1 {
                        Ok(a) => {
                            crate::dlogf!("SELFTEST turn1={:?}", a.text);
                            conv.push(ChatMessage::assistant(a.text));
                            conv.push(ChatMessage::user("explain"));
                            match ai::chat_stream(&provider, &conv, "off", |_| {}, |_| {}).await {
                                Ok(b) => crate::dlogf!("SELFTEST turn2={:?} turns={}", b.text, conv.len()),
                                Err(e) => crate::dlogf!("SELFTEST turn2 FAILED: {e}"),
                            }
                        }
                        Err(e) => crate::dlogf!("SELFTEST turn1 FAILED: {e}"),
                    }
                });
            }

            // `--preview` shows the toolbar with sample text, without needing a real
            // selection. Useful for inspecting the UI without taking over the user's mouse.
            if std::env::args().any(|a| a == "--preview") {
                PREVIEW.store(true, Ordering::SeqCst);
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(800)).await;
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

            // `--selftest-panels` opens two result panels from fake selections and logs the
            // resulting window labels. Multi-window support is otherwise only observable by
            // clicking the toolbar twice, which cannot be automated reliably.
            //
            // With `--selftest-stream` both panels also run a turn CONCURRENTLY against the
            // configured provider. That is the case that used to cross-talk: the listeners
            // were registered with target `Any`, so each window received the other window's
            // chunks and the second answer streamed into the first window too.
            if std::env::args().any(|a| a == "--selftest-panels") {
                let h = handle.clone();
                let stream_both = std::env::args().any(|a| a == "--selftest-stream");
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    let provider = store::load().providers.into_iter().next();
                    let mut labels = Vec::new();
                    for (i, text) in ["Rust", "JavaScript"].iter().enumerate() {
                        let use_provider = if stream_both { provider.clone() } else { None };
                        let pending = PendingAction {
                            action: json!({ "id": "translate", "name": format!("selftest {}", i + 1), "color": "#3b6fe0" }),
                            text: (*text).into(),
                            program_name: "selftest".into(),
                            system_prompt: "reply ok".into(),
                            provider: use_provider
                                .as_ref()
                                .map(|p| serde_json::to_value(p).unwrap_or(json!({}))),
                            model_name: use_provider.as_ref().map(|p| p.model.clone()).unwrap_or_default(),
                            thinking_level: "off".into(),
                            thinking_supported: false,
                            window_label: String::new(),
                        };
                        match new_panel(&h, pending) {
                            Ok(label) => {
                                crate::dlogf!("SELFTEST panel opened: {label} <- {text}");
                                labels.push(label);
                            }
                            Err(e) => crate::dlogf!("SELFTEST panel FAILED: {e}"),
                        }
                    }

                    // The renderer already starts its own turn when it loads and finds a
                    // provider in its context, so nothing is triggered here: the two panels
                    // simply stream concurrently, which is the case that used to cross-talk
                    // (listeners were registered with the unscoped target `Any`).
                    crate::dlogf!(
                        "SELFTEST panels now: {} {:?} (streaming={stream_both})",
                        labels.len(),
                        labels
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
            // A closed result panel releases its conversation, otherwise every rerun would
            // keep its history (and provider snapshot) alive for the whole session.
            tauri::WindowEvent::Destroyed => {
                let label = window.label().to_string();
                if label.starts_with("action") {
                    if let Some(app) = window.app_handle().try_state::<State>() {
                        let mut panels = app.panels.lock().unwrap();
                        panels.meta.remove(&label);
                        panels.conv.remove(&label);
                    }
                }
            }
            // Remember the panel's size so the next result opens at the size the user chose.
            tauri::WindowEvent::Resized(size) => {
                if window.label().starts_with("action") {
                    // `size` is physical; convert to logical for the builder.
                    let scale = window.scale_factor().unwrap_or(1.0).max(0.1);
                    if let Some(app) = window.app_handle().try_state::<State>() {
                        let mut slot = app.panel_size.lock().unwrap();
                        *slot = Some((size.width as f64 / scale, size.height as f64 / scale));
                    }
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error building tauri app")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                selection::cleanup();
            }
        });
}
