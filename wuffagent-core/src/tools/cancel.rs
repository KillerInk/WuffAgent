//! Per-tool-call cancellation.
//!
//! A running agent loop can execute several tool calls in parallel
//! (`agents::agent::tool_exec::PendingToolRuns`). Each in-flight call gets
//! its own [`CancellationToken`], registered here under a stable key
//! (`"{session_id}:{call_id}"`) so the UI can address ONE live tool card
//! individually (the card's Stop button). Tokens are children of the run's
//! token, so cancelling the whole run (Stop button / new message) cascades to
//! every in-flight call.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

/// Shared registry of in-flight tool calls' cancel tokens, keyed by
/// `"{session_id}:{call_id}"`.
///
/// Cheap to share: wrap in [`Arc`] and clone across threads (the agent loop,
/// each tool-execution task, and the UI all hold clones).
#[derive(Default)]
pub struct CancelRegistry {
    tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh token for `key` as a child of `parent` (when
    /// provided), replacing any previous token for the key. Returns the new
    /// token to hand to the tool.
    pub fn register(
        &self,
        key: &str,
        parent: Option<&CancellationToken>,
    ) -> CancellationToken {
        let token = match parent {
            Some(parent) => parent.child_token(),
            None => CancellationToken::new(),
        };
        self.tokens
            .lock()
            .unwrap()
            .insert(key.to_string(), token.clone());
        token
    }

    /// Cancel the tool call registered under `key`. Returns `false` when no
    /// call with that key is (any longer) running.
    pub fn cancel(&self, key: &str) -> bool {
        let tokens = self.tokens.lock().unwrap();
        match tokens.get(key) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Remove a finished call's token.
    pub fn deregister(&self, key: &str) {
        self.tokens.lock().unwrap().remove(key);
    }

    /// Whether a call with that key is still registered (running).
    pub fn is_live(&self, key: &str) -> bool {
        self.tokens.lock().unwrap().contains_key(key)
    }

    /// Cancel every in-flight call. Defensive — the run-level token cascade
    /// (child tokens) already does this when the whole run is cancelled.
    pub fn cancel_all(&self) {
        for token in self.tokens.lock().unwrap().values() {
            token.cancel();
        }
    }

    /// Number of in-flight calls (tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.tokens.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn register_cancel_deregister() {
        let reg = CancelRegistry::new();
        assert!(!reg.cancel("s:c1")); // nothing registered yet
        let token = reg.register("s:c1", None);
        assert!(reg.is_live("s:c1"));
        assert_eq!(reg.len(), 1);
        assert!(!token.is_cancelled());
        assert!(reg.cancel("s:c1"));
        assert!(token.is_cancelled());
        reg.deregister("s:c1");
        assert!(!reg.is_live("s:c1"));
        assert!(!reg.cancel("s:c1"));
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn parent_cancellation_cascades_to_children() {
        let reg = CancelRegistry::new();
        let run = CancellationToken::new();
        let tool_token = reg.register("s:c1", Some(&run));
        assert!(!tool_token.is_cancelled());
        run.cancel();
        assert!(tool_token.is_cancelled());
    }

    #[test]
    fn cancel_all_cancels_everything() {
        let reg = CancelRegistry::new();
        let a = reg.register("s:a", None);
        let b = reg.register("s:b", None);
        reg.cancel_all();
        assert!(a.is_cancelled() && b.is_cancelled());
    }

    #[test]
    fn re_register_replaces_previous_token() {
        let reg = CancelRegistry::new();
        let old = reg.register("s:c1", None);
        let new = reg.register("s:c1", None);
        assert_eq!(reg.len(), 1);
        assert!(reg.cancel("s:c1"));
        assert!(!old.is_cancelled());
        assert!(new.is_cancelled());
    }

    #[test]
    fn registry_is_send_sync_and_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<CancelRegistry>>();
    }
}
