//! Run-stats helpers. Split out of agents/agent.rs (A1).
//!
//! The trajectory counters themselves are accumulated by `run_llm_loop`
//! (see `loop.rs`) as the round progresses — `run_native_tool_calls` /
//! `run_text_embedded_calls` increment a `RunStats` per executed call.
//! (The old `run_stats_since` re-scan of the request list is gone: the
//! list is trimmed in place mid-run, which invalidated its start-of-run
//! offset and zeroed the counts on long turns.)

use super::Agent;
use crate::types::Message;

impl Agent {
    /// Extract the current turn's user request from the message history.
    ///
    /// Uses the LAST user message. Call it ONCE at the start of the run,
    /// before any verification nudge is pushed: after a NEEDS_FIX the last
    /// user message is the nudge, and re-extracting would make the judge
    /// grade the response against the nudge instead of the real request.
    /// (In a multi-turn session the first user message is stale for the same
    /// reason — the last one is the request being answered.)
    pub(crate) fn extract_original_request(&self, messages: &[Message]) -> String {
        messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }
}
