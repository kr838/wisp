//! Persisted settings (JSON in %APPDATA%\wisp\settings.json).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One toolbar action.
///
/// An action IS its prompt: `prompt` is the instruction sent to the model, and nothing else
/// describes what the action does. There is no action-type field — the two local actions that
/// never call a model (复制 / 搜索) are recognised by their fixed id, see `is_local_action`.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Action {
    pub id: String,
    pub name: String,
    /// Accent colour (hex) used to distinguish this action in the toolbar.
    #[serde(default = "default_color")]
    pub color: String,
    /// Kept only so older settings files (which had an emoji icon) still deserialize; the
    /// field is never written back.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    pub icon: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_inherit", rename = "thinkingLevel")]
    pub thinking_level: String,
    #[serde(default, rename = "providerId")]
    pub provider_id: Option<String>,
}

/// Ids of the two actions that run locally and never call a model.
pub const LOCAL_COPY: &str = "copy";
pub const LOCAL_SEARCH: &str = "search";

/// True for the built-in actions that run locally (copy to clipboard / open a search engine).
/// They are identified by id because they are created once and cannot be re-typed in the UI.
pub fn is_local_action(id: &str) -> bool {
    id == LOCAL_COPY || id == LOCAL_SEARCH
}

fn default_true() -> bool {
    true
}
fn default_inherit() -> String {
    "inherit".into()
}
fn default_color() -> String {
    "#3b6fe0".into()
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Provider {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "apiBase")]
    pub api_base: String,
    #[serde(default, rename = "apiKey")]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// "auto" | "on" | "off" — whether the model supports thinking.
    #[serde(default = "default_auto")]
    pub thinking: String,
    #[serde(default, rename = "thinkingLevel")]
    pub thinking_level: Option<String>,
    /// Extra HTTP headers sent with every request, as `Name: Value` pairs. Needed by some
    /// gateways (e.g. OpenCode Go requires a stable `x-opencode-session`).
    #[serde(default)]
    pub headers: Vec<HeaderPair>,
}

/// One custom request header.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

fn default_auto() -> String {
    "auto".into()
}

/// Every field carries a serde default so a settings file written by an older build (or one
/// missing a key) still loads instead of being rejected wholesale.
#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_selected", rename = "triggerMode")]
    pub trigger_mode: String,
    #[serde(default = "default_true", rename = "followToolbar")]
    pub follow_toolbar: bool,
    #[serde(default = "default_true", rename = "clipboardFallback")]
    pub clipboard_fallback: bool,
    #[serde(default = "default_default", rename = "filterMode")]
    pub filter_mode: String,
    #[serde(default, rename = "filterList")]
    pub filter_list: Vec<String>,
    #[serde(default = "default_light")]
    pub theme: String,
    /// Icon-only toolbar (no text labels), mirroring Cherry Studio's compact mode.
    #[serde(default)]
    pub compact: bool,
    /// Window backdrop material: "mica" | "mica_alt" | "acrylic" | "none".
    #[serde(default = "default_material")]
    pub material: String,
    /// Opacity of the tint laid over the material (0.15–0.95). Lower lets more of the
    /// backdrop through — read by the front-end as the `--glass-a` CSS variable.
    #[serde(default = "default_glass_opacity", rename = "glassOpacity")]
    pub glass_opacity: f64,
    /// Which light/dark variant the backdrop uses: "auto" | "light" | "dark".
    #[serde(default = "default_material_dark", rename = "materialDark")]
    pub material_dark: String,
    /// Stable per-install session id. Gateways such as OpenCode Go ask for one per
    /// conversation to route and cache efficiently; a constant value works and stays
    /// cacheable.
    #[serde(default = "random_id", rename = "sessionId")]
    pub session_id: String,
    /// Settings schema version, used to run one-off migrations exactly once.
    #[serde(default, rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default = "default_search_url", rename = "searchUrl")]
    pub search_url: String,
    /// Global default thinking level: auto | off | low | medium | high
    #[serde(default = "default_auto", rename = "thinkingLevel")]
    pub thinking_level: String,
    #[serde(default = "default_actions")]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default, rename = "defaultProviderId")]
    pub default_provider_id: String,
}

fn default_selected() -> String {
    "selected".into()
}
fn default_default() -> String {
    "default".into()
}
fn default_light() -> String {
    "light".into()
}
fn default_material() -> String {
    "mica".into()
}
fn default_glass_opacity() -> f64 {
    0.62
}
fn default_material_dark() -> String {
    "auto".into()
}
fn default_search_url() -> String {
    "https://www.bing.com/search?q={query}".into()
}

/// Bump when a one-off settings migration is added.
///   0 -> 1: append optional built-ins (copy/search) if missing.
///   1 -> 2: turn the optional built-ins off, so the toolbar shows only opt-in actions.
///   2 -> 3: fold the built-in prompts into each action's `prompt`, so the action editor
///           has a single prompt field and no action-type selector.
pub const CURRENT_SCHEMA: u32 = 3;

/// Short random identifier, used for the session id.
pub fn random_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let pid = std::process::id() as u128;
    let mixed = nanos ^ (pid << 32);
    format!("{mixed:032x}")[..32].to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            trigger_mode: default_selected(),
            follow_toolbar: true,
            clipboard_fallback: true,
            filter_mode: default_default(),
            filter_list: vec![],
            theme: default_light(),
            compact: false,
            material: default_material(),
            glass_opacity: default_glass_opacity(),
            material_dark: default_material_dark(),
            session_id: random_id(),
            schema_version: CURRENT_SCHEMA,
            search_url: default_search_url(),
            thinking_level: "auto".into(),
            actions: default_actions(),
            providers: vec![],
            default_provider_id: String::new(),
        }
    }
}

/// Rainbow palette: each built-in action gets its own hue so the bar stays flat and
/// text-only while remaining easy to scan.
pub const RAINBOW: &[&str] = &[
    "#3b6fe0", // blue
    "#7c3aed", // violet
    "#c2410c", // orange
    "#0f766e", // teal
    "#be185d", // rose
    "#4d7c0f", // olive
    "#0e7490", // cyan
    "#9333ea", // purple
];

fn default_actions() -> Vec<Action> {
    let mk = |id: &str, name: &str, color: &str, prompt: &str| Action {
        id: id.into(),
        name: name.into(),
        color: color.into(),
        icon: String::new(),
        prompt: prompt.into(),
        enabled: true,
        thinking_level: "inherit".into(),
        provider_id: None,
    };
    // The four presets ship with their prompt filled in; the user can edit that text
    // freely. The two local actions (copy/search) are present but OFF by default.
    let mut copy = mk(LOCAL_COPY, "复制", RAINBOW[4], "");
    copy.enabled = false;
    let mut search = mk(LOCAL_SEARCH, "搜索", RAINBOW[5], "");
    search.enabled = false;
    vec![
        mk("translate", "翻译", RAINBOW[0], crate::ai::PRESET_TRANSLATE),
        mk("summarize", "总结", RAINBOW[1], crate::ai::PRESET_SUMMARIZE),
        mk("explain", "解释", RAINBOW[2], crate::ai::PRESET_EXPLAIN),
        mk("rewrite", "润色", RAINBOW[3], crate::ai::PRESET_REWRITE),
        copy,
        search,
    ]
}

static STORE: Mutex<Option<Settings>> = Mutex::new(None);

pub fn path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("wisp").join("settings.json")
}

pub fn load() -> Settings {
    let mut guard = STORE.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        return s.clone();
    }

    let raw = std::fs::read_to_string(path()).ok();
    let mut broken = false;
    let mut s = match raw.as_deref().map(serde_json::from_str::<Settings>) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            // Never let a malformed file silently wipe the user's setup: keep a copy of
            // whatever was there, start from defaults, and tell the log.
            crate::dlogf!("[store] settings parse failed ({e}); file kept as .corrupt");
            let backup = path().with_extension(format!("json.corrupt-{}", timestamp()));
            let _ = std::fs::rename(path(), backup);
            broken = true;
            Settings::default()
        }
        None => Settings::default(),
    };

    let mut changed = broken;

    // v0 -> v1: ensure the optional built-ins exist so they appear in 设置 → 动作.
    for extra in default_actions() {
        if (extra.id == "search" || extra.id == "copy") && !s.actions.iter().any(|a| a.id == extra.id) {
            s.actions.push(extra);
            changed = true;
        }
    }

    // v1 -> v2: copy and search become strictly opt-in. They used to be on by default, so
    // existing installs must be switched off once — tracked by the schema version so a user
    // who later enables them is not fought by the migration on every launch.
    if s.schema_version < 2 {
        for a in s.actions.iter_mut() {
            if a.id == "copy" || a.id == "search" {
                a.enabled = false;
            }
        }
        s.schema_version = 2;
        changed = true;
    }

    // v2 -> v3: the action editor no longer has a type selector, and the persisted action no
    // longer carries a type at all. The presets keep their instructions in `prompt`, so an
    // existing install must have its preset text baked in once. Their ids are the old builtin
    // markers (`translate`, `summarize`, …), which is what makes this migration idempotent and
    // independent of the removed field.
    if s.schema_version < CURRENT_SCHEMA {
        for a in s.actions.iter_mut() {
            if !is_local_action(&a.id) && a.prompt.trim().is_empty() {
                a.prompt = crate::ai::builtin_prompt(&a.id).to_string();
            }
        }
        s.schema_version = CURRENT_SCHEMA;
        changed = true;
    }

    if changed {
        write_to_disk(&s);
    }

    *guard = Some(s.clone());
    s
}

fn timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn save(s: &Settings) {
    let mut guard = STORE.lock().unwrap();
    *guard = Some(s.clone());
    write_to_disk(s);
}

/// Serialize to the settings file WITHOUT taking the store lock.
///
/// Split out from `save` so callers that already hold the lock (notably `load`, which may
/// migrate the file) can persist without re-locking: `std::sync::Mutex` is not reentrant,
/// and calling `save` from inside `load` deadlocked the whole app on first read.
fn write_to_disk(s: &Settings) {
    if let Some(parent) = path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(path(), text);
    }
}

/// Merge a partial JSON patch into the current settings (shallow per top-level key).
pub fn merge(patch: &Value) -> Settings {
    let mut cur = serde_json::to_value(load()).unwrap_or(json!({}));
    if let (Some(cur_map), Some(patch_map)) = (cur.as_object_mut(), patch.as_object()) {
        for (k, v) in patch_map {
            cur_map.insert(k.clone(), v.clone());
        }
    }
    // NEVER fall back to `Settings::default()` here: a single unexpected shape would then
    // silently wipe every provider and action, which looks exactly like "saving does not
    // work". On failure keep the previous settings and leave the file untouched.
    match serde_json::from_value::<Settings>(cur) {
        Ok(next) => {
            save(&next);
            next
        }
        Err(e) => {
            crate::dlogf!("[store] rejected settings patch ({e}); keeping previous settings");
            load()
        }
    }
}

pub fn reset() -> Settings {
    let s = Settings::default();
    save(&s);
    s
}
