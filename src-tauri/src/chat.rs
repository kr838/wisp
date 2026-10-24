//! Conversation message type.
//!
//! Kept in its own dependency-free module so the panel state machine can be tested without
//! the HTTP client or the Tauri runtime.

use serde::{Deserialize, Serialize};

/// One message in a conversation.
///
/// Follow-up questions need the whole history, so requests are built from a message list
/// rather than a fixed (system, user) pair.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}
