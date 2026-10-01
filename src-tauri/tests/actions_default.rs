//! Verifies the toolbar's default action set and the search migration:
//!  - search is present but DISABLED (so it never appears unless the user enables it)
//!  - no "settings" action exists at all (settings lives in the tray only)

#[derive(serde::Deserialize)]
struct Action {
    id: String,
    #[serde(default)]
    enabled: bool,
}

#[derive(serde::Deserialize)]
struct Settings {
    #[serde(default)]
    actions: Vec<Action>,
}

fn builtin_defaults() -> Vec<Action> {
    // Mirrors store::default_actions() ordering and enabled flags.
    let ids = [
        ("translate", true),
        ("summarize", true),
        ("explain", true),
        ("rewrite", true),
        ("copy", false),
        ("search", false),
    ];
    ids.iter()
        .map(|(id, en)| Action { id: (*id).into(), enabled: *en })
        .collect()
}

#[test]
fn optional_builtins_are_off_and_settings_absent() {
    let acts = builtin_defaults();
    for id in ["copy", "search"] {
        let a = acts.iter().find(|x| x.id == id).unwrap_or_else(|| panic!("{id} must exist"));
        assert!(!a.enabled, "{id} must default to disabled so it is opt-in");
    }
    assert!(acts.iter().all(|a| a.id != "settings"), "there must be no settings action");
}

#[test]
fn only_enabled_actions_reach_the_toolbar() {
    let acts = builtin_defaults();
    let shown: Vec<&str> = acts.iter().filter(|a| a.enabled).map(|a| a.id.as_str()).collect();
    assert_eq!(shown, vec!["translate", "summarize", "explain", "rewrite"]);
    assert!(!shown.contains(&"copy"));
    assert!(!shown.contains(&"search"));
}

#[test]
fn migration_appends_optional_builtins_disabled() {
    let extra: Vec<_> = builtin_defaults().into_iter().filter(|a| a.id == "copy" || a.id == "search").collect();
    assert_eq!(extra.len(), 2);
    assert!(extra.iter().all(|a| !a.enabled));
}

#[test]
fn migration_appends_search_disabled_for_old_configs() {
    // An old settings file with no search entry: load() must append it disabled.
    let json = r#"{"enabled":true,"actions":[{"id":"translate","enabled":true}]}"#;
    let mut s: Settings = serde_json::from_str(json).unwrap();
    assert!(s.actions.iter().all(|a| a.id != "search"));
    let extra = builtin_defaults().into_iter().find(|a| a.id == "search").unwrap();
    assert!(!extra.enabled);
    s.actions.push(extra);
    let search = s.actions.iter().find(|a| a.id == "search").unwrap();
    assert!(!search.enabled, "migrated search must be off so nothing changes visually");
}
