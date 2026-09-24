//! Pure conversation-`Vec<Message>` helpers for the chat client.
//!
//! The session *persistence* orchestration (save/load/retry/failure-queue)
//! moved to `crate::sessions::persist` (Phase 2, E2a) so this module no longer
//! depends on `crate::sessions`. What remains here only manipulates the
//! in-memory conversation buffer and takes persistence as an injected closure.

use std::sync::{Arc, Mutex};

use crate::types::Message;

/// Trim the conversation to the given max_messages, preserving the system message.
pub fn trim_conversation(conversation: &Arc<Mutex<Vec<Message>>>, max_messages: usize) {
    let mut conv = conversation.lock().unwrap();
    let initial_len = conv.len();
    if initial_len <= max_messages {
        return;
    }
    // The client conversation has no stored system prompt (it is prepended at
    // request-build time). Protect a leading system message only if one is
    // actually present at index 0 — tool results mislabelled "system" further
    // back must remain removable, otherwise trimming keeps a huge prefix.
    let keep_from = if conv.first().map(|m| m.role.as_str()) == Some("system") {
        1
    } else {
        0
    };
    let trim_at = conv.len().saturating_sub(max_messages);
    if trim_at > keep_from {
        conv.drain(keep_from..trim_at);
        tracing::info!(
            "trimming: trim_conversation (count) removed {} messages ({} -> {}, max={})",
            trim_at - keep_from,
            initial_len,
            conv.len(),
            max_messages
        );
    }
}

/// Clear all messages from the conversation.
pub fn clear_history(conversation: &Arc<Mutex<Vec<Message>>>) {
    conversation.lock().unwrap().clear();
}

/// Clear all messages from the conversation and save the (empty) session.
pub fn clear_session_messages(
    conversation: &Arc<Mutex<Vec<Message>>>,
    save_session_fn: &dyn Fn() -> Result<(), anyhow::Error>,
) {
    conversation.lock().unwrap().clear();
    if let Err(e) = save_session_fn() {
        tracing::error!("Failed to save session after clear: {}", e);
    }
}

#[cfg(test)]
mod tests;
