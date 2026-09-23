//! The message-sweep machinery of `ContextTrimming`: protected-tail
//! detection, tool-pair handling, age-based removal, in-place tool-result
//! summarization, and the last-resort largest-message truncation.
//! Split out of `summarizer/mod.rs` (Phase D size split).

use std::collections::HashSet;

use crate::types::Message;

use super::{filestate, TrimConfig, MIN_TRUNCATED_CONTENT_CHARS, TRUNCATED_PLACEHOLDER};

impl super::ContextTrimming {
    /// Index of the start of the "protected tail": the current round the model
    /// has produced but not yet read. That is the LATER of (a) the last user
    /// message and (b) the last assistant message carrying tool calls, so the
    /// fresh assistant tool-call + its tool results are always included.
    /// Everything from this index to the end must be shown to the model in full
    /// — trimming must never remove or shrink it, so a fresh tool result is
    /// always read by the AI before it can be trimmed. Older rounds (before this
    /// index) are free to be removed or summarized.
    pub(super) fn protected_tail_start(messages: &[Message]) -> usize {
        let last_user = messages.iter().rposition(|m| m.role == "user");
        let last_toolcall = messages
            .iter()
            .rposition(|m| m.role == "assistant" && m.tool_calls.is_some());
        match (last_user, last_toolcall) {
            (Some(u), Some(t)) => u.max(t),
            (Some(u), None) => u,
            (None, Some(t)) => t,
            (None, None) => messages.len(),
        }
    }

    /// Exclusive end index of the tool pair starting at `i`.
    ///
    /// A "tool pair" is an assistant message carrying `tool_calls` together
    /// with the contiguous run of `role: "tool"` results that immediately
    /// follow it. Removing that whole span keeps call/result pairing intact
    /// (no orphaned `tool_call_id`, no dangling call). For a lone
    /// `role: "tool"` message with no preceding assistant in the span
    /// (defensive; not produced by well-formed history) the pair is just that
    /// single message.
    pub(super) fn tool_pair_end(messages: &[Message], i: usize) -> usize {
        let is_assistant_call = messages[i].role == "assistant" && messages[i].tool_calls.is_some();
        let mut j = i + 1;
        if is_assistant_call {
            while j < messages.len() && messages[j].role == "tool" {
                j += 1;
            }
        }
        j.max(i + 1)
    }

    /// True if the message at `i` participates in a tool pair: an assistant
    /// message carrying `tool_calls`, or a tool result whose `tool_call_id`
    /// matches one of them. Removing participants must remove the whole pair
    /// as a unit (see `tool_pair_end`).
    pub(super) fn is_paired_at(messages: &[Message], i: usize) -> bool {
        let m = &messages[i];
        if m.role == "assistant" {
            return m.tool_calls.is_some();
        }
        if m.role == "tool" {
            if let Some(id) = m.tool_call_id.as_deref() {
                return messages
                    .iter()
                    .filter(|a| a.role == "assistant")
                    .filter_map(|a| a.tool_calls.as_ref())
                    .flatten()
                    .any(|tc| tc.id == id);
            }
        }
        false
    }

    /// Age-based removal sweep: drop oldest messages — tool pairs (assistant
    /// call + its results) as a unit, plain turns singly — until the list
    /// fits `target_chars` or the protected tail is reached.
    ///
    /// Pairs containing a tool result whose call id is in `protected_reads`
    /// (the CURRENT snapshot of a file the model read multiple times or that
    /// is large — see the freshness pass of `trim_messages`) are skipped by
    /// this sweep: they are only removed once everything else is gone, so
    /// the model keeps its working snapshot of actively-edited files as long
    /// as the budget allows. `trim_messages` runs a second sweep with an
    /// empty set when the budget still cannot be met, so the trim always
    /// converges. Returns the number of messages removed.
    pub(super) fn age_sweep(
        messages: &mut Vec<Message>,
        target_chars: usize,
        start: usize,
        protected_reads: &HashSet<String>,
    ) -> usize {
        let mut removed = 0;
        let mut keep_from = start;
        loop {
            if Self::message_char_count(messages) <= target_chars {
                break;
            }
            if keep_from >= messages.len() {
                break;
            }
            // Recompute the protected tail each iteration: removing a message
            // before it shifts the tail index down, so a stale value would let
            // the pointer run past it and orphan the fresh tool results.
            let protect_from = Self::protected_tail_start(messages);
            // Stop before the protected tail: everything from the last user
            // message / last assistant tool-call onward is the current round
            // the AI has not read yet.
            if keep_from >= protect_from {
                break;
            }
            // Never remove the last user message — in agent mode it is the
            // task the whole conversation is about, and the server requires at
            // least one user message (a 500 if the last message is not a user).
            // Skip it (advance keep_from) so we can still trim messages AFTER it.
            if let Some(lui) = messages.iter().rposition(|m| m.role == "user") {
                if keep_from == lui {
                    keep_from += 1;
                    continue;
                }
            }
            // Tool pairs (assistant with tool_calls + its tool results): remove
            // the entire pair as a unit. The model has already consumed these
            // results in an older round, so they are safe to drop. Removing the
            // assistant AND all its tool results together keeps pairing intact.
            if Self::is_paired_at(messages, keep_from) {
                let pair_end = Self::tool_pair_end(messages, keep_from);
                if !protected_reads.is_empty()
                    && (keep_from..pair_end).any(|j| {
                        messages[j].role == "tool"
                            && messages[j]
                                .tool_call_id
                                .as_ref()
                                .is_some_and(|id| protected_reads.contains(id))
                    })
                {
                    // This round carries the model's working snapshot of a file
                    // it is actively editing: defer it (a second sweep removes
                    // it if the budget cannot be met any other way).
                    keep_from = pair_end;
                    continue;
                }
                messages.drain(keep_from..pair_end);
                removed += pair_end - keep_from;
                continue;
            }
            // Plain user/assistant turn: remove it.
            messages.remove(keep_from);
            removed += 1;
        }
        removed
    }

    /// Last-resort shrink: halve the largest messages (measured by
    /// `message_tokens`, i.e. content + reasoning + tool-call args — the same
    /// metric the budget uses) until the total fits `target_tokens` or no
    /// candidate has content left. Messages whose content reaches the floor are
    /// set to a placeholder and skipped, so no message — in particular no
    /// `role: "tool"` result — ever ends up with empty content.
    /// `min_shrink_chars`: only messages with content LONGER than this are
    /// candidates. Collapsing a message at the floor writes the 41-char
    /// placeholder, so shrinking a message of 41 chars or fewer GROWS the
    /// total — pass 0 for pre-tail rounds (existing behavior) and the
    /// placeholder length for the last-resort tail stage, so the fresh user
    /// request is never destroyed for no savings.
    pub(super) fn truncate_largest_message(
        messages: &mut [Message],
        target_tokens: usize,
        protect_from: usize,
        never_shrink_reads: bool,
        min_shrink_chars: usize,
    ) -> bool {
        let leading_system = messages.first().map(|m| m.role.as_str()) == Some("system");
        let read_calls = if never_shrink_reads {
            Some(filestate::call_map(messages))
        } else {
            None
        };
        let mut truncated = false;
        loop {
            if Self::message_char_count(messages) <= target_tokens {
                break;
            }
            // Pick the largest shrinkable candidate: real content, not already
            // reduced to the placeholder, never the leading system prompt, and
            // never inside the protected tail.
            let best = (0..messages.len())
                .filter(|&i| !(i == 0 && leading_system))
                .filter(|&i| i < protect_from)
                .filter(|&i| messages[i].content.chars().count() > min_shrink_chars)
                .filter(|&i| messages[i].content != TRUNCATED_PLACEHOLDER)
                // A halved file snapshot invites the model to hallucinate
                // line contents it no longer has — file content is either
                // fully present or fully absent.
                .filter(|&i| {
                    !never_shrink_reads
                        || read_calls
                            .as_ref()
                            .map(|c| !filestate::is_read_file_result(c, &messages[i]))
                            .unwrap_or(true)
                })
                .max_by_key(|&i| Self::message_tokens(&messages[i]));
            let Some(idx) = best else { break };
            let current_chars = messages[idx].content.chars().count();
            if current_chars <= MIN_TRUNCATED_CONTENT_CHARS {
                // At the floor: collapse to the placeholder and move on.
                messages[idx].content = TRUNCATED_PLACEHOLDER.to_string();
                truncated = true;
                continue;
            }
            // Halve (snap to a char boundary); stop at the floor.
            let target_chars = (current_chars / 2).max(MIN_TRUNCATED_CONTENT_CHARS);
            if target_chars >= current_chars {
                messages[idx].content = TRUNCATED_PLACEHOLDER.to_string();
                truncated = true;
                continue;
            }
            let byte_pos = messages[idx]
                .content
                .char_indices()
                .take(target_chars)
                .last()
                .map(|(b, _)| b)
                .unwrap_or(0);
            messages[idx].content.truncate(byte_pos);
            truncated = true;
        }
        truncated
    }

    /// Shrink oversized `role: "tool"` messages that sit BEFORE the protected
    /// tail (i.e. rounds the model has already consumed) by summarizing them in
    /// place via the type-aware summarizers. This preserves tool_call/tool_call_id
    /// pairing (no message is removed) and never yields empty content: the
    /// summarizers always return non-empty text for non-empty input, and the
    /// final generic hard cap keeps the result within `budget_chars`.
    /// The fresh round (protected tail) is left untouched so the AI always
    /// reads the latest tool output in full.
    pub(super) fn summarize_old_tool_messages(
        &self,
        messages: &mut [Message],
        protect_from: usize,
        budget_chars: usize,
        config: &TrimConfig,
        never_shrink_reads: bool,
    ) {
        let budget = budget_chars.max(MIN_TRUNCATED_CONTENT_CHARS * 4);
        // With freshness eviction on, `read_file` results are skipped: a
        // partially summarized file snapshot invites the model to hallucinate
        // line contents it no longer has, so file content is either fully
        // present or fully absent.
        let read_calls = if never_shrink_reads {
            Some(filestate::call_map(messages))
        } else {
            None
        };
        // Single pass over the pre-tail messages, largest first: each tool
        // result is summarized at most once (bounded, no re-selection of a
        // message that could not be shrunk), so this always terminates.
        let mut candidates: Vec<usize> = (0..protect_from)
            .filter(|&i| messages[i].role == "tool" && messages[i].content.len() > budget)
            .filter(|&i| {
                !never_shrink_reads
                    || read_calls
                        .as_ref()
                        .map(|c| !filestate::is_read_file_result(c, &messages[i]))
                        .unwrap_or(true)
            })
            .collect();
        candidates.sort_by_key(|&i| std::cmp::Reverse(Self::message_tokens(&messages[i])));
        for idx in candidates {
            if Self::message_char_count(messages) <= budget_chars {
                break;
            }
            let original = messages[idx].content.clone();
            let summarized = self.summarize_to_budget(&original, budget, config);
            if summarized.len() >= original.len() {
                continue; // could not shrink this one; move on
            }
            tracing::info!(
                "trimming: summarized old tool result in place (len: {} -> {})",
                original.len(),
                summarized.len()
            );
            messages[idx].content = summarized;
        }
    }
}
