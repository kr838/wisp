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
    // Buttons inside the header sit within the drag region, so each must opt out explicitly.
    // Only the header is inspected: the footer's buttons are outside the drag region.
    let header: String = html
        .split("<header")
        .nth(1)
        .and_then(|s| s.split("</header>").next())
        .unwrap_or_default()
        .to_string();
    let buttons: Vec<&str> = header.lines().filter(|l| l.contains("<button")).collect();
    for b in &buttons {
        assert!(
            b.contains(r#"data-tauri-drag-region="false""#),
            "header button must opt out of dragging: {}",
            b.trim()
        );
    }
    // An undecorated window has no native title bar, so all three controls must exist.
    for id in ["btn-min", "btn-max", "btn-close"] {
        assert!(
            html.contains(&format!(r#"id="{id}""#)),
            "the header must provide {id}"
        );
    }
    // 复制结果 was removed: it duplicated selecting the text.
    assert!(
        !html.contains("btn-copy"),
        "the result window must not offer a copy button"
    );
}
