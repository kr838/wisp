//! Per-panel conversation state for the result windows.
//!
//! Every result window (`action-1`, `action-2`, …) owns an independent conversation. Keeping
//! this as a plain state machine (no Tauri types) makes the behaviours that matter testable:
//!
//! * opening a second panel must not touch the first one's history;
//! * a reply that arrives after the user stopped or re-asked must be dropped, not appended;
//! * stopping or retrying must roll the history back to a *valid* prefix — never to a state
//!   that lost the selected text, and never one ending in an unanswered question.
//!
//! The last point is why a turn records a snapshot of the conversation it started from: a
//! failed or stopped turn restores that snapshot instead of trying to guess which message to
//! pop (the first turn's "question" is the selected text itself and must survive).

use std::collections::HashMap;

use crate::chat::ChatMessage;

#[derive(Default)]
pub struct Panel {
    /// [system, user, assistant, user, ...]
    conversation: Vec<ChatMessage>,
    /// The messages the first turn started with, kept so 重试 can replay it exactly.
    initial: Vec<ChatMessage>,
    /// Conversation as it was when the current turn began, for failure/stop rollback.
    turn_base: Vec<ChatMessage>,
    streaming: bool,
    /// Incremented on every new request; a stream whose generation is stale is ignored, so a
    /// stopped turn cannot append its answer after a newer one started.
    generation: u64,
}

#[derive(Default)]
pub struct Panels {
    map: HashMap<String, Panel>,
}

/// The query helpers are used by the panel tests, which compile this module directly.
#[allow(dead_code)]
impl Panels {
    /// Create the conversation for a new panel: system prompt + the selected text.
    pub fn open(&mut self, label: &str, system: &str, text: &str) {
        let mut p = Panel::default();
        p.conversation.push(ChatMessage::system(system));
        p.conversation.push(ChatMessage::user(text));
        p.initial = p.conversation.clone();
        self.map.insert(label.to_string(), p);
    }

    /// Start (or retry) the first turn. The conversation is reset to the initial messages so a
    /// retry cannot append the same question twice.
    pub fn begin(&mut self, label: &str) -> Option<(u64, Vec<ChatMessage>)> {
        let p = self.map.get_mut(label)?;
        p.conversation = p.initial.clone();
        p.turn_base = p.initial.clone();
        Some(start_turn(p))
    }

    /// Append a follow-up question and start streaming it.
    pub fn ask(&mut self, label: &str, text: &str) -> Option<(u64, Vec<ChatMessage>)> {
        let p = self.map.get_mut(label)?;
        p.turn_base = p.conversation.clone();
        p.conversation.push(ChatMessage::user(text));
        Some(start_turn(p))
    }

    /// Record a successful answer. Returns false when the turn was superseded (stopped, or a
    /// newer question was asked) and the reply must be ignored.
    pub fn finish(&mut self, label: &str, generation: u64, text: &str) -> bool {
        let Some(p) = self.map.get_mut(label) else { return false };
        if p.generation != generation {
            return false;
        }
        p.streaming = false;
        p.conversation.push(ChatMessage::assistant(text));
        true
    }

    /// Record a failed turn: restore the history to before it started, so a retry does not
    /// send the question twice and the next follow-up is not preceded by an unanswered one.
    pub fn fail(&mut self, label: &str, generation: u64) -> bool {
        let Some(p) = self.map.get_mut(label) else { return false };
        if p.generation != generation {
            return false;
        }
        p.streaming = false;
        p.conversation = p.turn_base.clone();
        true
    }

    /// Stop the in-flight turn. Its partial answer is discarded by the UI, so the history rolls
    /// back to the state before the turn. The generation bump invalidates the HTTP response
    /// when it finally arrives.
    pub fn stop(&mut self, label: &str) {
        if let Some(p) = self.map.get_mut(label) {
            // A stray stop (double click, or a stop after the answer already arrived) must not
            // delete the answer that was just committed.
            if !p.streaming {
                return;
            }
            p.streaming = false;
            p.conversation = p.turn_base.clone();
            p.generation = p.generation.wrapping_add(1);
        }
    }

    pub fn remove(&mut self, label: &str) {
        self.map.remove(label);
    }

    pub fn conversation(&self, label: &str) -> Option<&[ChatMessage]> {
        self.map.get(label).map(|p| p.conversation.as_slice())
    }

    pub fn is_streaming(&self, label: &str) -> bool {
        self.map.get(label).map(|p| p.streaming).unwrap_or(false)
    }

    pub fn labels(&self) -> Vec<&str> {
        self.map.keys().map(|s| s.as_str()).collect()
    }
}

/// Start a turn and return the generation it owns plus a snapshot of the messages.
fn start_turn(p: &mut Panel) -> (u64, Vec<ChatMessage>) {
    p.streaming = true;
    p.generation = p.generation.wrapping_add(1);
    (p.generation, p.conversation.clone())
}
