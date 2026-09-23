/// Type-specific summarizers for the trimming pipeline.
///
/// Each summarizer knows how to compress a particular content type
/// while preserving key information (paths, error messages, line counts).
use super::classifier::{classify_content, ContentType};
use super::config::TrimConfig;
use super::filestate;
use crate::types::Message;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

mod kinds;
pub use kinds::*;

/// Token count of a message string: 1 char = 1 token unit (exact, no estimation).
/// Since real tokens are sub-strings, tokens <= chars, so this is always an
/// upper bound on the true token count — trimming triggers early rather than
/// letting the server reject an over-budget request.
pub fn estimate_tokens(text: &str) -> usize {
    ContextTrimming::estimate_tokens(text)
}

/// Token count of a single message: content + reasoning content + tool-call
/// arguments, each counted by `estimate_tokens` (1 char = 1 unit).
pub fn message_tokens(m: &Message) -> usize {
    ContextTrimming::message_tokens(m)
}

/// Total token count of a message list (content + reasoning + tool args).
pub fn message_char_count(messages: &[Message]) -> usize {
    ContextTrimming::message_char_count(messages)
}

/// Floor for in-place truncation: a message's content is never shrunk below
/// this many chars (it is set to the placeholder at exactly this length) so
/// that no message — especially a `role: "tool"` result — ever ends up with
/// empty content that the LLM/server rejects.
const MIN_TRUNCATED_CONTENT_CHARS: usize = 80;

/// Placeholder written when a message's content is shrunk to the truncation floor.
const TRUNCATED_PLACEHOLDER: &str = "[content trimmed to fit context window]";

/// The main trimming engine that dispatches to the right summarizer.
#[derive(Clone, Default)]
pub struct ContextTrimming {
    _private: (),
}

impl ContextTrimming {
    /// Create a new ContextTrimming instance.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Summarize a tool result based on its content type.
    ///
    /// If the result is within the configured `max_tool_result_chars`,
    /// returns it unchanged. Otherwise, applies the appropriate summarizer.
    pub fn summarize_tool_result(&self, content: &str, config: &TrimConfig) -> String {
        if !config.is_enabled() {
            return content.to_string();
        }
        self.summarize_to_budget(content, config.max_tool_result_chars, config)
    }

    /// Summarize content to fit within an explicit character budget.
    ///
    /// Returns the content unchanged if it already fits within the budget.
    /// Dispatches to the type-specific summarizer, with a final generic
    /// hard cap as a fallback.
    pub fn summarize_to_budget(
        &self,
        content: &str,
        budget_chars: usize,
        config: &TrimConfig,
    ) -> String {
        // Hard cap: always enforce the max.
        if content.len() <= budget_chars {
            return content.to_string();
        }

        let content_type = classify_content(content);
        tracing::info!(
            "trimming: summarizing content (type={:?}, len={}, budget={})",
            content_type,
            content.len(),
            budget_chars
        );
        let result = match content_type {
            ContentType::BuildLog => BuildLogSummarizer.summarize(content, budget_chars, config),
            ContentType::SourceCode => CodeSummarizer.summarize(content, budget_chars, config),
            ContentType::FileList => ListSummarizer.summarize(content, budget_chars, config),
            ContentType::SearchResults => {
                SearchResultsSummarizer.summarize(content, budget_chars, config)
            }
            ContentType::ToolError => ErrorSummarizer.summarize(content, budget_chars, config),
            ContentType::JsonWrapper | ContentType::FreeText => {
                GenericSummarizer.summarize(content, budget_chars, config)
            }
        };

        // Final hard cap after summarization.
        if result.len() > budget_chars {
            tracing::warn!(
                "trimming: post-summarize still over budget (len={}, budget={}), applying hard cap",
                result.len(),
                budget_chars
            );
            GenericSummarizer.summarize(&result, budget_chars, config)
        } else {
            result
        }
    }

    /// Token count of a message string: 1 char = 1 token unit (exact, no estimation).
    /// Since real tokens are sub-strings, tokens <= chars, so this is always an
    /// upper bound on the true token count — trimming triggers early rather than
    /// letting the server reject an over-budget request.
    pub fn estimate_tokens(text: &str) -> usize {
        text.chars().count()
    }

    /// Token count of a single message: content + reasoning content + tool-call
    /// arguments, each counted by `estimate_tokens` (1 char = 1 unit).
    pub fn message_tokens(m: &Message) -> usize {
        let mut total = Self::estimate_tokens(&m.content);
        if let Some(ref rc) = m.reasoning_content {
            total += Self::estimate_tokens(rc);
        }
        if let Some(ref tcs) = m.tool_calls {
            for tc in tcs {
                total += Self::estimate_tokens(&tc.function.arguments);
            }
        }
        total
    }

    /// Total token count of a message list (content + reasoning + tool args).
    pub fn message_char_count(messages: &[Message]) -> usize {
        messages.iter().map(Self::message_tokens).sum()
    }

    /// Index of the start of the "protected tail": the current round the model
    /// has produced but not yet read. That is the LATER of (a) the last user
    /// message and (b) the last assistant message carrying tool calls, so the
    /// fresh assistant tool-call + its tool results are always included.
    /// Everything from this index to the end must be shown to the model in full
    /// — trimming must never remove or shrink it, so a fresh tool result is
    /// always read by the AI before it can be trimmed. Older rounds (before this
    /// index) are free to be removed or summarized.
    fn protected_tail_start(messages: &[Message]) -> usize {
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
    fn tool_pair_end(messages: &[Message], i: usize) -> usize {
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
    fn is_paired_at(messages: &[Message], i: usize) -> bool {
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
    fn age_sweep(
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
    fn truncate_largest_message(
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
    fn summarize_old_tool_messages(
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

    /// Token-budget trim: remove oldest non-system messages until the token
    /// count (chars of all messages incl. reasoning and tool-call args) is below
    /// the target. Returns the number of messages removed.
    pub fn trim_messages(
        &self,
        messages: &mut Vec<Message>,
        target_chars: usize,
        config: &TrimConfig,
    ) -> usize {
        if !config.is_enabled() {
            return 0;
        }
        if messages.is_empty() {
            return 0;
        }

        let mut removed = 0;

        // ── Freshness pass: fully remove stale/superseded file-read pairs ──
        // Runs BEFORE the age-based removal. A stale file snapshot (the file
        // was modified after the read, or a newer read supersedes it) is
        // content the model can no longer trust — kept even as a one-line
        // marker it invites the model to hallucinate line contents it no
        // longer has, so the WHOLE tool pair (assistant call + tool result)
        // is removed. If the file still matters, the model re-reads it.
        // Pre-tail only.
        //
        // The same pass decides which CURRENT reads the age-based removal
        // below must DEFER: the model is actively working on files it read
        // multiple times (or that are large), and if their latest snapshot
        // is trimmed away it edits from a stale memory of the file and only
        // notices ("search text not found") after a failed apply_diff.
        let mut protected_reads: HashSet<String> = HashSet::new();
        if config.stale_file_invalidation {
            let index = filestate::build_file_state_index(messages);
            if !index.last_read.is_empty() {
                let protect_from = Self::protected_tail_start(messages);
                for (path, &i) in &index.last_read {
                    if i >= protect_from {
                        continue; // inside the protected tail: already safe
                    }
                    let m = &messages[i];
                    if filestate::is_file_marker(&m.content) {
                        continue; // already collapsed: no working snapshot
                    }
                    if index.last_mutate.get(path).is_some_and(|&mi| mi > i) {
                        continue; // stale: the freshness pass removes it
                    }
                    if filestate::is_protected_read(&index, path, &m.content) {
                        if let Some(id) = m.tool_call_id.clone() {
                            protected_reads.insert(id);
                        }
                    }
                }
                let dropped = filestate::remove_stale_read_pairs(messages, protect_from, &index);
                if dropped > 0 {
                    removed += dropped;
                    tracing::info!(
                        "[TRIM] freshness pass: removed {dropped} message(s) from stale/superseded read_file pair(s)"
                    );
                }
            }
        }

        let keep_from = if messages.first().map(|m| m.role.as_str()) == Some("system") {
            1
        } else {
            0
        };

        if keep_from >= messages.len() {
            return 0;
        }

        let initial_count = messages.len();
        let initial_chars = Self::message_char_count(messages);
        // The current round (last user message / last assistant tool-call and
        // the tool results after it) is never removed or shrunk: the AI must
        // read fresh tool output before it can be trimmed.
        let protect_from = Self::protected_tail_start(messages);
        tracing::info!(
            "trimming: trim_messages called (messages={}, chars={}, target={}, keep_from={}, protect_from={}, protected_reads={})",
            initial_count,
            initial_chars,
            target_chars,
            keep_from,
            protect_from,
            protected_reads.len()
        );

        // Age-based removal: oldest first, tool pairs as units, stopping
        // before the protected tail. Pairs holding a protected current file
        // snapshot (see the freshness pass) are deferred to a second sweep.
        removed += Self::age_sweep(messages, target_chars, keep_from, &protected_reads);
        if Self::message_char_count(messages) > target_chars {
            // Still over budget: the protected snapshots can no longer defer
            // removal — sweep again without protection (oldest first) so the
            // trim always converges.
            removed += Self::age_sweep(messages, target_chars, keep_from, &HashSet::new());
        }

        // Second pass: compress already-consumed tool rounds in place
        // (summarization keeps paths/errors/line counts and never empties).
        // With freshness eviction on, `read_file` results are excluded: a
        // partially summarized/halved file snapshot is worse than none, so
        // file content is either fully present or fully absent.
        let protect_from = Self::protected_tail_start(messages);
        let never_shrink_reads = config.stale_file_invalidation;
        self.summarize_old_tool_messages(
            messages,
            protect_from,
            target_chars,
            config,
            never_shrink_reads,
        );

        // Fallback: if still over budget, truncate the largest shrinkable
        // message (protected tail excluded) to force it under.
        Self::truncate_largest_message(messages, target_chars, protect_from, never_shrink_reads, 0);

        // Last resort: the protected tail ITSELF is over budget — a single
        // huge fresh tool result or a pasted user message. The fresh round is
        // normally never removed or shrunk (the model must read fresh output
        // in full), but an unshrinkable tail means the request overflows
        // n_ctx and the server rejects it outright — a halved snapshot beats
        // a hard failure. This stage also relaxes the read_file exemption:
        // the fresh read's pair cannot be removed (that would orphan the
        // live tool call), so halving is the only in-place option left.
        // Tail messages at or below the placeholder length are left alone:
        // collapsing them would grow the total, never shrink it.
        if Self::message_char_count(messages) > target_chars {
            let all = messages.len();
            Self::truncate_largest_message(
                messages,
                target_chars,
                all,
                false,
                TRUNCATED_PLACEHOLDER.chars().count(),
            );
        }

        let final_count = messages.len();
        let final_chars = Self::message_char_count(messages);
        tracing::info!(
            "trimming: trim_messages done (removed={}, messages: {} -> {}, chars: {} -> {})",
            removed,
            initial_count,
            final_count,
            initial_chars,
            final_chars
        );

        removed
    }

    /// Token-budget trim for Arc<Mutex<Vec<Message>>> (chat client conversation).
    pub fn trim_conversation(
        &self,
        conversation: &Arc<Mutex<Vec<Message>>>,
        target_chars: usize,
        config: &TrimConfig,
    ) -> usize {
        if !config.is_enabled() {
            return 0;
        }
        let mut conv = conversation.lock().unwrap();
        self.trim_messages(&mut conv, target_chars, config)
    }

    /// Token-budget trim: remove oldest non-system messages until the token
    /// count is below the target. Legacy wrapper — delegates to `trim_messages`.
    /// `target_tokens` is in char units (same as `estimate_tokens` returns chars).
    pub fn trim_to_token_budget(
        &self,
        messages: &mut Vec<Message>,
        target_tokens: usize,
        config: &TrimConfig,
    ) -> usize {
        self.trim_messages(messages, target_tokens, config)
    }
}

#[cfg(test)]
mod tests;
