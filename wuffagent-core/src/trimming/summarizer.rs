/// Type-specific summarizers for the trimming pipeline.
///
/// Each summarizer knows how to compress a particular content type
/// while preserving key information (paths, error messages, line counts).
use super::classifier::{classify_content, ContentType};
use super::config::TrimConfig;
use super::filestate;
use crate::types::Message;
use std::sync::{Arc, Mutex};

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

/// Minimum lines to keep from build logs when summarizing.
const BUILD_LOG_MIN_KEEP: usize = 3;

/// Minimum lines to keep from source code when summarizing.
const CODE_MIN_KEEP: usize = 5;

/// Default estimated average line length in characters.
const DEFAULT_AVG_LINE_LEN: usize = 30;

/// Reserved character budget for the "omitted" indicator text.
const OMIT_TEXT_RESERVED_CHARS: usize = 50;

/// Minimum items to keep from lists when summarizing.
const LIST_MIN_KEEP: usize = 2;

/// Floor for in-place truncation: a message's content is never shrunk below
/// this many chars (it is set to the placeholder at exactly this length) so
/// that no message — especially a `role: "tool"` result — ever ends up with
/// empty content that the LLM/server rejects.
const MIN_TRUNCATED_CONTENT_CHARS: usize = 80;

/// Placeholder written when a message's content is shrunk to the truncation floor.
const TRUNCATED_PLACEHOLDER: &str = "[content trimmed to fit context window]";

/// A trait for content-specific summarizers.
pub trait ContentSummarizer: Send + Sync {
    /// Summarize the given content, respecting the budget.
    fn summarize(&self, content: &str, budget_chars: usize, config: &TrimConfig) -> String;
}

/// Summarizer for build logs (cargo, rustc output).
/// Keeps first/last N lines and summarizes the middle.
#[derive(Default)]
pub struct BuildLogSummarizer;

impl ContentSummarizer for BuildLogSummarizer {
    fn summarize(&self, content: &str, _budget_chars: usize, config: &TrimConfig) -> String {
        let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let total = lines.len();
        let max_lines = config.log_max_lines;

        if total <= max_lines {
            return content.to_string();
        }

        let keep_each = max_lines.saturating_sub(1) / 2;
        let keep_each = keep_each.max(BUILD_LOG_MIN_KEEP);

        let mut result: Vec<String> = Vec::new();
        result.extend(lines.iter().take(keep_each).cloned());
        result.push(format!(
            "<{} lines omitted>",
            total.saturating_sub(keep_each * 2)
        ));
        result.extend(lines.iter().skip(total.saturating_sub(keep_each)).cloned());

        result.join("\n")
    }
}

/// Summarizer for source code.
/// Keeps first/last N lines with a code-style ellipsis in the middle.
#[derive(Default)]
pub struct CodeSummarizer;

impl ContentSummarizer for CodeSummarizer {
    fn summarize(&self, content: &str, _budget_chars: usize, config: &TrimConfig) -> String {
        let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let total = lines.len();
        let max_lines = config.code_max_lines;

        if total <= max_lines {
            return content.to_string();
        }

        let keep_each = max_lines.saturating_sub(1) / 2;
        let keep_each = keep_each.max(CODE_MIN_KEEP);

        let mut result: Vec<String> = Vec::new();
        result.extend(lines.iter().take(keep_each).cloned());
        result.push(format!(
            "// ... <{} lines omitted> ...",
            total.saturating_sub(keep_each * 2)
        ));
        result.extend(lines.iter().skip(total.saturating_sub(keep_each)).cloned());

        result.join("\n")
    }
}

/// Summarizer for file/glob lists.
/// Keeps first N items and last M items.
#[derive(Default)]
pub struct ListSummarizer;

impl ContentSummarizer for ListSummarizer {
    fn summarize(&self, content: &str, budget_chars: usize, config: &TrimConfig) -> String {
        let items: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let total = items.len();
        let max_items = config.list_max_items;

        if total <= max_items {
            return content.to_string();
        }

        // Budget-aware: estimate per-line cost and cap total items to fit budget.
        // Typical line: ~20-40 chars + newline. Reserve ~40 chars for the omitted line.
        let sample_lines: Vec<usize> = items.iter().take(5).map(|l| l.len()).collect();
        let avg_line_len = if sample_lines.is_empty() {
            DEFAULT_AVG_LINE_LEN
        } else {
            sample_lines.iter().sum::<usize>() / sample_lines.len()
        };
        let line_cost = avg_line_len + 1; // +1 for newline
        let reserved_for_omit = OMIT_TEXT_RESERVED_CHARS;
        let available = budget_chars.saturating_sub(reserved_for_omit);
        let budget_items = (available / line_cost).max(3);

        let max_keep = budget_items.min(max_items);
        let keep_front = (max_keep / 2).max(LIST_MIN_KEEP);
        let keep_back = max_keep - keep_front;

        let mut result: Vec<String> = Vec::new();
        result.extend(
            items
                .iter()
                .copied()
                .map(|s| s.to_string())
                .take(keep_front),
        );
        result.push(format!(
            "// ... <{} items omitted> ...",
            total.saturating_sub(keep_front + keep_back)
        ));
        result.extend(
            items
                .iter()
                .copied()
                .map(|s| s.to_string())
                .skip(total.saturating_sub(keep_back)),
        );

        result.join("\n")
    }
}

/// Summarizer for search results.
/// Keeps the query and top results, truncates the rest.
#[derive(Default)]
pub struct SearchResultsSummarizer;

impl ContentSummarizer for SearchResultsSummarizer {
    fn summarize(&self, content: &str, budget_chars: usize, config: &TrimConfig) -> String {
        let trimmed = content.trim();
        if !trimmed.starts_with('{') {
            // Fallback to generic summarizer.
            return GenericSummarizer.summarize(content, budget_chars, config);
        }

        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => {
                let query = value
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or("unknown");
                let results = value
                    .get("results")
                    .and_then(|r| r.as_array())
                    .cloned()
                    .unwrap_or_default();

                let total = results.len();
                let keep = 3.min(total);

                let kept: Vec<&serde_json::Value> = results.iter().take(keep).collect();
                let mut result = serde_json::json!({
                    "query": query,
                    "total_results": total,
                    "results": kept,
                    "truncated": total > keep,
                    "omitted_count": total.saturating_sub(keep),
                });

                // Preserve other fields from the original.
                for (k, v) in value.as_object().unwrap() {
                    if k != "results" && k != "query" {
                        result[k] = v.clone();
                    }
                }

                serde_json::to_string_pretty(&result).unwrap_or_else(|_| content.to_string())
            }
            Err(_) => GenericSummarizer.summarize(content, budget_chars, config),
        }
    }
}

/// Summarizer for tool errors.
/// Keeps the error message, drops stack traces.
#[derive(Default)]
pub struct ErrorSummarizer;

impl ContentSummarizer for ErrorSummarizer {
    fn summarize(&self, content: &str, budget_chars: usize, _config: &TrimConfig) -> String {
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return String::new();
        }

        // Take only the first line (the actual error message).
        let first = lines[0].trim();
        if first.is_empty() {
            return content.to_string();
        }

        // If the first line is short enough, just return it.
        if first.len() <= budget_chars {
            first.to_string()
        } else {
            format!("{}...", &first[..budget_chars])
        }
    }
}

/// Generic summarizer for free text and JSON wrappers.
/// Character-count based truncation with ellipsis.
#[derive(Default)]
pub struct GenericSummarizer;

impl ContentSummarizer for GenericSummarizer {
    fn summarize(&self, content: &str, budget_chars: usize, _config: &TrimConfig) -> String {
        if content.len() <= budget_chars {
            return content.to_string();
        }

        // Keep first and last portion, with ellipsis in the middle.
        // Snap byte offsets to char boundaries to avoid panics on
        // multi-byte UTF-8 content.
        let head = budget_chars / 2;
        let tail = budget_chars / 2;
        let head_end = content.floor_char_boundary(head);
        let tail_start = content.ceil_char_boundary(content.len().saturating_sub(tail));
        if tail_start <= head_end {
            return content.to_string();
        }
        format!(
            "{} ... [...] ... {}",
            &content[..head_end],
            &content[tail_start..]
        )
    }
}

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

    /// Last-resort shrink: halve the largest messages (measured by
    /// `message_tokens`, i.e. content + reasoning + tool-call args — the same
    /// metric the budget uses) until the total fits `target_tokens` or no
    /// candidate has content left. Messages whose content reaches the floor are
    /// set to a placeholder and skipped, so no message — in particular no
    /// `role: "tool"` result — ever ends up with empty content.
    fn truncate_largest_message(
        messages: &mut [Message],
        target_tokens: usize,
        protect_from: usize,
        never_shrink_reads: bool,
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
                .filter(|&i| !messages[i].content.is_empty())
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
        if config.stale_file_invalidation {
            let index = filestate::build_file_state_index(messages);
            if !index.last_read.is_empty() {
                let protect_from = Self::protected_tail_start(messages);
                let dropped = filestate::remove_stale_read_pairs(messages, protect_from, &index);
                if dropped > 0 {
                    removed += dropped;
                    tracing::info!(
                        "[TRIM] freshness pass: removed {dropped} message(s) from stale/superseded read_file pair(s)"
                    );
                }
            }
        }

        let mut keep_from = if messages.first().map(|m| m.role.as_str()) == Some("system") {
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
            "trimming: trim_messages called (messages={}, chars={}, target={}, keep_from={}, protect_from={})",
            initial_count,
            initial_chars,
            target_chars,
            keep_from,
            protect_from
        );

        // `is_paired` marks tool-call participants at index `i` (an assistant
        // message carrying tool_calls, or a tool result whose tool_call_id
        // matches one). Recomputed each removal because indices shift.
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

        loop {
            let prompt_len = Self::message_char_count(messages);
            if prompt_len <= target_chars {
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
            // message / last assistant tool-call onward is the current round the
            // AI has not read yet.
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
            if is_paired_at(messages, keep_from) {
                let pair_end = Self::tool_pair_end(messages, keep_from);
                messages.drain(keep_from..pair_end);
                removed += pair_end - keep_from;
                continue;
            }
            // Plain user/assistant turn: remove it.
            messages.remove(keep_from);
            removed += 1;
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
        Self::truncate_largest_message(messages, target_chars, protect_from, never_shrink_reads);

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
