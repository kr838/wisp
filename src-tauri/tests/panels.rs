//! Panel (result window) state-machine regressions.
//!
//! Covers the two behaviours the multi-window support depends on:
//!  * each panel owns an independent conversation (opening a second one never touches the
//!    first);
//!  * a reply that arrives after the user stopped or re-asked is dropped.
//!
//! `panels` only depends on `chat`, so both are compiled from source with `#[path]`. That
//! avoids linking the Tauri runtime, which cannot start in every environment (it needs
//! WebView2Loader beside a binary living in `deps/`).

// The two modules are declared at this crate's root, so the `crate::chat` path used inside
// `panels.rs` resolves to exactly the same declaration.
#[path = "../src/chat.rs"]
mod chat;
#[path = "../src/panels.rs"]
mod panels;

use chat::ChatMessage;
use panels::Panels;

fn roles(msgs: &[ChatMessage]) -> Vec<String> {
    msgs.iter().map(|m| m.role.clone()).collect()
}

#[test]
fn two_panels_keep_independent_histories() {
    let mut p = Panels::default();
    p.open("action-1", "sys A", "selection A");
    p.open("action-2", "sys B", "selection B");

    let (g1, msgs1) = p.begin("action-1").unwrap();
    let (g2, msgs2) = p.begin("action-2").unwrap();
    assert_eq!(roles(&msgs1), vec!["system", "user"]);
    assert_eq!(msgs1[1].content, "selection A");
    assert_eq!(msgs2[1].content, "selection B");
    // Generations are per panel, so both start at 1; the isolation that matters is that
    // finishing one panel's turn does not touch the other.
    assert_eq!((g1, g2), (1, 1));

    assert!(p.finish("action-1", g1, "answer A"));
    assert!(!p.is_streaming("action-1"));
    // The other panel is untouched and still streaming its own turn.
    assert!(p.is_streaming("action-2"));
    assert_eq!(p.conversation("action-1").unwrap().last().unwrap().content, "answer A");
    assert_eq!(roles(p.conversation("action-2").unwrap()), vec!["system", "user"]);
}

#[test]
fn follow_up_appends_to_the_same_panel_only() {
    let mut p = Panels::default();
    p.open("action-1", "sys", "text");
    p.open("action-2", "sys", "other");

    let (g, _) = p.begin("action-1").unwrap();
    p.finish("action-1", g, "first answer");

    let (_, msgs) = p.ask("action-1", "follow up?").unwrap();
    assert_eq!(roles(&msgs), vec!["system", "user", "assistant", "user"]);
    assert_eq!(msgs[3].content, "follow up?");
    // action-2 never saw the first panel's turn.
    assert_eq!(roles(p.conversation("action-2").unwrap()), vec!["system", "user"]);
}

#[test]
fn stop_discards_the_partial_question_and_late_reply() {
    let mut p = Panels::default();
    p.open("action-1", "sys", "text");
    let (g, _) = p.begin("action-1").unwrap();

    p.stop("action-1");
    // The question that produced the discarded answer must not remain in the history.
    assert_eq!(roles(p.conversation("action-1").unwrap()), vec!["system", "user"]);
    assert!(!p.is_streaming("action-1"));

    // The HTTP response arrives afterwards: it must not be appended.
    assert!(!p.finish("action-1", g, "late answer"));
    assert_eq!(roles(p.conversation("action-1").unwrap()), vec!["system", "user"]);
}

#[test]
fn retry_does_not_duplicate_the_selection_text() {
    let mut p = Panels::default();
    p.open("action-1", "sys", "the text");
    let (g1, _) = p.begin("action-1").unwrap();
    p.fail("action-1", g1);

    let (g2, msgs) = p.begin("action-1").unwrap();
    assert_eq!(roles(&msgs), vec!["system", "user"], "retry must replay the first turn");
    assert_eq!(msgs[1].content, "the text");
    assert_ne!(g1, g2);
}

#[test]
fn failure_pops_only_the_unanswered_question() {
    let mut p = Panels::default();
    p.open("action-1", "sys", "text");
    let (g, _) = p.begin("action-1").unwrap();
    p.finish("action-1", g, "ok");

    let (g2, _) = p.ask("action-1", "second question").unwrap();
    p.fail("action-1", g2);
    assert_eq!(
        roles(p.conversation("action-1").unwrap()),
        vec!["system", "user", "assistant"],
        "the successful turn must survive"
    );
}

#[test]
fn closed_panel_forgets_everything() {
    let mut p = Panels::default();
    p.open("action-1", "sys", "text");
    p.remove("action-1");
    assert!(p.conversation("action-1").is_none());
    assert!(p.begin("action-1").is_none());
    assert!(p.labels().is_empty());
}

#[test]
fn stray_stop_keeps_the_committed_answer() {
    let mut p = Panels::default();
    p.open("action-1", "sys", "text");
    let (g, _) = p.begin("action-1").unwrap();
    p.finish("action-1", g, "answer");
    // Pressing 停止 after the turn already completed must not roll the answer back.
    p.stop("action-1");
    assert_eq!(
        roles(p.conversation("action-1").unwrap()),
        vec!["system", "user", "assistant"]
    );
}
