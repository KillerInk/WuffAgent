//! Run stats extraction. Split out of agents/agent.rs (A1).

use super::Agent;
use crate::agents::types::RunStats;
use crate::types::Message;

impl Agent {

    /// I1: count tool calls and errors in `messages[from..]` (this run's
    /// portion only — earlier turns of a multi-turn conversation are excluded)
    /// and combine with the verification attempts into `RunStats`.
    ///
    /// Tool errors are detected by the "Error: " prefix the loop writes into
    /// failed tool results; a successful tool output that merely STARTS with
    /// that text (rare) is over-counted — acceptable for advisory evidence.
    pub(crate) fn run_stats_since(messages: &[Message], from: usize, verification_attempts: u32) -> RunStats {
        let mut tool_calls = 0usize;
        let mut tool_errors = 0usize;
        for m in messages.iter().skip(from) {
            if let Some(calls) = &m.tool_calls {
                tool_calls += calls.len();
            }
            if m.role == "tool" && m.content.starts_with("Error: ") {
                tool_errors += 1;
            }
        }
        RunStats {
            tool_calls,
            tool_errors,
            verification_attempts,
        }
    }

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
