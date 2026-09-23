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
mod trim;

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
