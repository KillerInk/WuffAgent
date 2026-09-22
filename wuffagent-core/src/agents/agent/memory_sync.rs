//! Conversation-store to memory-store reconciliation.
//! Split out of agents/agent.rs (A1).

use super::Agent;
use super::VERIFICATION_NUDGE;
use crate::types::Message;

impl Agent {

    /// Whether a message belongs in the shared store (vs. request-only).
    ///
    /// System messages (the per-run system prompt) and the verification retry
    /// nudge belong only in outgoing requests and are never persisted. Empty
    /// assistant placeholders (no content, no tool calls) are a streaming
    /// artifact and are never stored either.
    ///
    /// The nudge is recognized by its content AND its empty timestamp: the
    /// loop pushes it with `timestamp: String::new()`, while real user
    /// messages always carry `format_timestamp()`. Matching content alone
    /// would drop a user message that happens to repeat the nudge verbatim.
    pub(crate) fn is_storable(msg: &Message) -> bool {
        if msg.role == "system" {
            return false;
        }
        if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
            return false;
        }
        !(msg.role == "user" && msg.content == VERIFICATION_NUDGE && msg.timestamp.is_empty())
    }

    /// Reconcile the shared store with the per-run request list after a trim.
    ///
    /// The store is the single source of truth, but on the agent path only
    /// the throwaway request list is trimmed — without this, the store (and
    /// the session file) grows without bound. The request list is the store's
    /// storable projection plus request-only entries (system prompt, nudge),
    /// so replacing the store with that projection drops exactly the messages
    /// the trim removed and keeps store and request list in sync for every
    /// later turn (including after a session reload).
    pub(crate) fn reconcile_store(&self, messages: &[Message]) {
        let projected: Vec<Message> = messages
            .iter()
            .filter(|m| Self::is_storable(m))
            .cloned()
            .collect();
        let conv = self.client.conversation();
        *conv.lock().unwrap() = projected;
    }

    /// Record a generated message in the shared store, exactly once.
    ///
    /// Request-only messages (system prompt, verification nudge — see
    /// `is_storable`) are skipped. This mirrors the client's non-agent path,
    /// where the user and assistant messages are written to the shared
    /// conversation as they happen and the system prompt is never stored.
    pub(crate) fn record_in_store(&self, msg: &Message) {
        if !Self::is_storable(msg) {
            return;
        }
        let conv = self.client.conversation();
        conv.lock().unwrap().push(msg.clone());
    }
}
