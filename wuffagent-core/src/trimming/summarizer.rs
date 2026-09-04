/// Type-specific summarizers for the trimming pipeline.
///
/// Each summarizer knows how to compress a particular content type
/// while preserving key information (paths, error messages, line counts).

use super::classifier::{classify_content, ContentType};
use super::config::TrimConfig;
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
        result.push(format!("<{} lines omitted>", total.saturating_sub(keep_each * 2)));
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
        result.push(format!("// ... <{} lines omitted> ...", total.saturating_sub(keep_each * 2)));
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
        result.extend(items.iter().copied().map(|s| s.to_string()).take(keep_front));
        result.push(format!("// ... <{} items omitted> ...", total.saturating_sub(keep_front + keep_back)));
        result.extend(items.iter().copied().map(|s| s.to_string()).skip(total.saturating_sub(keep_back)));

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
    pub fn summarize_to_budget(&self, content: &str, budget_chars: usize, config: &TrimConfig) -> String {
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
            ContentType::SearchResults => SearchResultsSummarizer.summarize(content, budget_chars, config),
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

    /// Truncate agent chain entries to the configured max.
    pub fn truncate_agent_chain(&self, entries: &mut Vec<crate::sessions::model::AgentChainEntry>, config: &TrimConfig) {
        if !config.is_enabled() {
            return;
        }
        let max = config.max_chain_entries;
        if entries.len() > max {
            entries.drain(..entries.len() - max);
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
    ) -> bool {
        let leading_system = messages.first().map(|m| m.role.as_str()) == Some("system");
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
    ) {
        let budget = budget_chars.max(MIN_TRUNCATED_CONTENT_CHARS * 4);
        // Single pass over the pre-tail messages, largest first: each tool
        // result is summarized at most once (bounded, no re-selection of a
        // message that could not be shrunk), so this always terminates.
        let mut candidates: Vec<usize> = (0..protect_from)
            .filter(|&i| {
                messages[i].role == "tool" && messages[i].content.len() > budget
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

        let keep_from = if messages.first().map(|m| m.role.as_str()) == Some("system") { 1 } else { 0 };

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

        let mut removed = 0;
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
            // In that case the summarization/truncation passes handle the budget.
            if let Some(lui) = messages.iter().rposition(|m| m.role == "user") {
                if keep_from >= lui {
                    break;
                }
            }
            // Tool rounds are NEVER removed: an assistant message carrying
            // tool_calls and its `role: "tool"` results stay in place so the
            // call/result pairing is intact and no empty tool message is ever
            // produced. They are instead compressed in place by the summarization
            // pass below (and the truncation fallback as a last resort).
            if is_paired_at(messages, keep_from) {
                break;
            }
            // Only plain user/assistant turns are removed.
            messages.remove(keep_from);
            removed += 1;
        }

        // Second pass: compress already-consumed tool rounds in place
        // (summarization keeps paths/errors/line counts and never empties).
        let protect_from = Self::protected_tail_start(messages);
        self.summarize_old_tool_messages(messages, protect_from, target_chars, config);

        // Fallback: if still over budget, truncate the largest shrinkable
        // message (protected tail excluded) to force it under.
        Self::truncate_largest_message(messages, target_chars, protect_from);

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
mod tests {
    use super::*;
    use super::super::config::TrimConfig;

    fn make_config() -> TrimConfig {
        TrimConfig {
            enabled: true,
            max_tool_result_chars: 200,
            max_chain_entries: 50,
            code_max_lines: 10,
            log_max_lines: 8,
            list_max_items: 10,
        }
    }

    #[test]
    fn test_build_log_summarization() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!("Compiling item {} ... ok", i));
        }
        let content = lines.join("\n");
        let trimming = ContextTrimming::new();
        let result = trimming.summarize_tool_result(&content, &make_config());

        assert!(result.contains("lines omitted"));
        assert!(result.len() <= 200);
        assert!(result.starts_with("Compiling item 0"));
        assert!(result.contains("Compiling item 49"));
    }

    #[test]
    fn test_code_summarization() {
        let mut lines = Vec::new();
        for i in 0..100 {
            lines.push(format!("    let x{} = {};", i, i));
        }
        let content = lines.join("\n");
        let trimming = ContextTrimming::new();
        let result = trimming.summarize_tool_result(&content, &make_config());

        assert!(result.contains("lines omitted"));
        assert!(result.len() <= 200);
        assert!(result.starts_with("    let x0"));
    }

    #[test]
    fn test_small_content_not_truncated() {
        let content = "short result";
        let trimming = ContextTrimming::new();
        let result = trimming.summarize_tool_result(content, &make_config());
        assert_eq!(result, "short result");
    }

    #[test]
    fn test_disabled_config_returns_unmodified() {
        let content = "x".repeat(5000);
        let mut config = make_config();
        config.enabled = false;
        let trimming = ContextTrimming::new();
        let result = trimming.summarize_tool_result(&content, &config);
        assert_eq!(result.len(), 5000);
    }

    #[test]
    fn test_file_list_summarization() {
        let paths: Vec<String> = (0..100)
            .map(|i| format!("/path/to/file_{}.rs", i))
            .collect();
        let content = paths.join("\n");
        let trimming = ContextTrimming::new();
        let result = trimming.summarize_tool_result(&content, &make_config());

        assert!(result.contains("items omitted"));
        assert!(result.len() <= 200);
    }

    fn user_msg(text: &str) -> Message {
        Message {
            role: "user".into(),
            content: text.into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: text.into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn assistant_tool_call(call_id: &str, args: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: call_id.into(),
                call_type: "function".into(),
                function: crate::types::ToolFunction {
                    name: "echo".into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn tool_result(call_id: &str, text: &str) -> Message {
        Message {
            role: "tool".into(),
            content: text.into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            reasoning_content: None,
        }
    }

    /// Every assistant tool-call id must have a matching tool result present in
    /// the trimmed list, and every tool result's id must match a present call.
    fn assert_pairs_intact(messages: &[Message]) {
        let call_ids: std::collections::HashSet<&str> = messages
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|tc| tc.id.as_str())
            .collect();
        for m in messages.iter().filter(|m| m.role == "tool") {
            let id = m.tool_call_id.as_deref().unwrap_or("");
            assert!(
                call_ids.contains(id),
                "tool result {} present without its paired tool call",
                id
            );
        }
    }

    #[test]
    fn test_trim_keeps_tool_pair_straddling_boundary_intact() {
        // system, then an early user/assistant pair, then an assistant tool
        // call whose result immediately follows the last user message. The
        // trim target forces removals that land right on the call/result
        // boundary — the pair must survive together.
        let mut messages = vec![
            Message {
                role: "system".into(),
                content: "sys".into(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            user_msg("first question"),
            assistant_msg("first answer"),
            user_msg("second question"),
            assistant_tool_call("call_1", "{\"n\":42}"),
            tool_result("call_1", "42"),
            user_msg("third question"),
        ];

        let trimming = ContextTrimming::new();
        // Target just above the system+last-user cost so the removal loop runs
        // and its pointer reaches the tool cluster.
        let target = 60;
        trimming.trim_messages(&mut messages, target, &make_config());

        assert_pairs_intact(&messages);
        // The last user message must always survive.
        assert!(messages.iter().any(|m| m.content == "third question"));
    }

    #[test]
    fn test_trim_multiple_user_messages_keeps_last_user_intact() {
        // Several user messages separated by assistant replies plus a tool
        // cluster near the end. Aggressive trimming must keep removing
        // messages (recomputing the last-user bound each iteration) and must
        // never drop the final user message.
        let mut messages = vec![
            Message {
                role: "system".into(),
                content: "sys".into(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            user_msg("q1"),
            assistant_msg("a1"),
            user_msg("q2"),
            assistant_msg("a2"),
            user_msg("q3"),
            assistant_tool_call("c1", "{}"),
            tool_result("c1", "ok"),
            user_msg("q4-final"),
        ];

        let trimming = ContextTrimming::new();
        // Target just below the total content but above the system + final
        // user floor: the removal loop must keep removing across multiple
        // user messages, recomputing the last-user bound each iteration, and
        // stop only once it would reach q4-final.
        let removed = trimming.trim_messages(&mut messages, 15, &make_config());

        assert!(removed > 0, "expected some messages to be removed");
        assert_pairs_intact(&messages);
        let last = messages.last().unwrap();
        assert_eq!(last.role, "user", "final message must stay a user message");
        assert_eq!(last.content, "q4-final");
    }

    #[test]
    fn test_protected_tail_start_prefers_latest_round() {
        // Agent loop shape: one user turn, then several tool rounds. The
        // protected tail must start at the LAST assistant tool call, not at the
        // (much earlier) user message — otherwise every old tool result would
        // be protected and never summarized.
        let messages = vec![
            user_msg("q"),
            assistant_tool_call("c1", "{}"),
            tool_result("c1", "old-1"),
            assistant_tool_call("c2", "{}"),
            tool_result("c2", "old-2"),
            assistant_tool_call("c3", "{}"),
            tool_result("c3", "fresh"),
        ];
        // index 5 = the last assistant tool call
        assert_eq!(ContextTrimming::protected_tail_start(&messages), 5);
    }

    #[test]
    fn test_protected_tail_start_plain_chat_uses_last_user() {
        let messages = vec![user_msg("q1"), assistant_msg("a1"), user_msg("q2")];
        assert_eq!(ContextTrimming::protected_tail_start(&messages), 2);
    }

    #[test]
    fn test_fresh_tool_result_untouched_by_trim() {
        // The tool result of the current round (after the last assistant
        // tool call) must come back byte-identical, even when the older part
        // of the conversation is far over budget.
        let big = (0..500).map(|i| format!("old line {}", i)).collect::<Vec<_>>().join("\n");
        let fresh = "FRESH-RESULT-EXACTLY-AS-IS";
        let mut messages = vec![
            user_msg("q"),
            assistant_tool_call("c1", "{}"),
            tool_result("c1", &big),
            assistant_tool_call("c2", "{}"),
            tool_result("c2", fresh),
        ];

        let trimming = ContextTrimming::new();
        // target tiny: forces removal/summarization of everything before the tail.
        trimming.trim_messages(&mut messages, 64, &make_config());

        let last = messages.last().unwrap();
        assert_eq!(last.role, "tool");
        assert_eq!(last.content, fresh, "fresh tool result must be untouched");
    }

    #[test]
    fn test_old_tool_result_summarized_in_place_non_empty() {
        // An over-budget tool result from an EARLIER round must be compressed
        // in place (pairing preserved, no message removed, content never empty).
        let big = (0..500).map(|i| format!("old line {}", i)).collect::<Vec<_>>().join("\n");
        let fresh = "fresh";
        let mut messages = vec![
            user_msg("q"),
            assistant_tool_call("c1", "{}"),
            tool_result("c1", &big),
            assistant_tool_call("c2", "{}"),
            tool_result("c2", fresh),
        ];
        let before_len = messages.len();

        let trimming = ContextTrimming::new();
        trimming.trim_messages(&mut messages, 256, &make_config());

        // No message dropped: pairing intact.
        assert_eq!(messages.len(), before_len);
        let old = &messages[2];
        assert_eq!(old.role, "tool");
        assert!(!old.content.is_empty(), "summarized tool result must not be empty");
        assert!(old.content.len() < big.len(), "old tool result should have been shrunk");
        // Fresh result still intact.
        assert_eq!(messages.last().unwrap().content, fresh);
    }

    #[test]
    fn test_no_empty_content_under_extreme_overage() {
        // With a target smaller than the system+tail floor, the fallback
        // truncation must run down to the placeholder floor — never to empty —
        // on the oversized non-tail messages.
        let big = "x".repeat(20_000);
        let mut messages = vec![
            Message {
                role: "system".into(),
                content: "sys".into(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            user_msg("q"),
            assistant_tool_call("c1", "{}"),
            tool_result("c1", &big),
            assistant_tool_call("c2", "{}"),
            tool_result("c2", "fresh"),
        ];

        let trimming = ContextTrimming::new();
        trimming.trim_messages(&mut messages, 100, &make_config());

        // No tool message may end up empty (that is what the agent/server
        // rejects). Assistant tool-call messages legitimately carry empty
        // content by design, so they are excluded from this check.
        for m in &messages {
            if m.role == "tool" {
                assert!(
                    !m.content.is_empty(),
                    "tool message ended up with empty content"
                );
            }
        }
        // The oversized old tool result must have been shrunk (it cannot fit).
        assert!(messages[3].content.len() < big.len());
        // Fresh result untouched.
        assert_eq!(messages.last().unwrap().content, "fresh");
    }
}
