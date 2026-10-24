//! Static contracts for the renderer.
//!
//! The result window once registered its event listeners with `listen(name, fn)`, which
//! targets `{kind:"Any"}`. Tauri's router matches `Any` unconditionally, so every result
//! window received every other window's stream chunks: with two panels open, the second
//! answer was also appended to the first. Scoping each listener to the current window label
//! fixed it, and this test makes sure a future edit cannot quietly drop the target again.

use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

#[test]
fn result_window_listeners_are_scoped_to_their_own_window() {
    let js = read("../ui/action/action.js");

    // The helper must exist and must pass a target.
    assert!(
        js.contains("getCurrentWindow") && js.contains(".label"),
        "action.js must derive its own window label"
    );
    assert!(
        js.contains("target: myLabel"),
        "the listener helper must pass {{ target: myLabel }} so events stay in this window"
    );

    // Every action-* listener has to go through that helper. A bare `listen('action-...')`
    // would be an unscoped, cross-window listener.
    for line in js.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("listen('action") {
            panic!("unscoped listener found (use onOwn instead): {trimmed}");
        }
    }
    assert!(
        js.contains("onOwn('action-chunk'"),
        "the streaming listener must be window-scoped"
    );
}

#[test]
fn header_is_a_deep_drag_region_with_clickable_buttons() {
    let html = read("../ui/action/index.html");
    assert!(
        html.contains(r#"class="head" data-tauri-drag-region="deep""#),
        "the whole header must be draggable; the bare attribute only covered the header's own padding"
    );
    // Window buttons sit inside the drag region, so they must opt out explicitly. Count only
    // button elements: the surrounding comment also mentions the attribute.
    let opt_out = html
        .lines()
        .filter(|l| l.contains("<button") && l.contains(r#"data-tauri-drag-region="false""#))
        .count();
    assert_eq!(
        opt_out, 2,
        "both window buttons must opt out of dragging so they stay clickable"
    );
}
