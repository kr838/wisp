//! Round-trip check: the exact JSON shape the settings window sends must deserialize back
//! into Settings. `store::merge` uses `from_value(...).unwrap_or_default()`, so a shape
//! mismatch would silently reset every provider — indistinguishable from "cannot save".

use serde_json::json;

// Mirror of the relevant structs (kept in sync with store.rs) to test the serde contract.
#[derive(serde::Serialize, serde::Deserialize)]
struct Provider {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "apiBase")]
    api_base: String,
    #[serde(default, rename = "apiKey")]
    api_key: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default = "auto")]
    thinking: String,
    #[serde(default, rename = "thinkingLevel")]
    thinking_level: Option<String>,
}
fn auto() -> String {
    "auto".into()
}

#[test]
fn provider_roundtrip_keeps_values() {
    // What the renderer actually POSTs back after the user types.
    let payload = json!([{
        "id": "prov-abc",
        "name": "OpenCode",
        "apiBase": "https://opencode.ai/zen/v1",
        "apiKey": "sk-test",
        "model": "gpt-5",
        "temperature": 0.7,
        "thinking": "auto"
    }]);
    let v: Vec<Provider> = serde_json::from_value(payload).expect("providers must deserialize");
    assert_eq!(v[0].api_key, "sk-test");
    assert_eq!(v[0].model, "gpt-5");
    assert_eq!(v[0].api_base, "https://opencode.ai/zen/v1");
}

#[test]
fn provider_tolerates_missing_optionals() {
    // A freshly added provider row (no apiKey/model yet) must still parse.
    let payload = json!([{
        "id": "prov-new",
        "name": "",
        "apiBase": "",
        "apiKey": "",
        "model": "",
        "temperature": 0.7,
        "thinking": "auto"
    }]);
    let v: Vec<Provider> = serde_json::from_value(payload).expect("new provider must parse");
    assert!(v[0].max_tokens.is_none());
    assert!(v[0].thinking_level.is_none());
}

#[test]
fn provider_tolerates_null_optional_numbers() {
    // settings_get serializes Option::None as null; sending that back must not fail.
    let payload = json!([{
        "id": "p", "name": "n", "apiBase": "b", "apiKey": "k", "model": "m",
        "temperature": null, "max_tokens": null, "thinking": "auto", "thinkingLevel": null
    }]);
    let v: Vec<Provider> = serde_json::from_value(payload).expect("nulls must parse");
    assert!(v[0].temperature.is_none());
}
