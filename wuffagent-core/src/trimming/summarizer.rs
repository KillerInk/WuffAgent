/// Type-specific summarizers for the trimming pipeline.
///
/// Each summarizer knows how to compress a particular content type
/// while preserving key information (paths, error messages, line counts).

use super::classifier::{classify_content, ContentType};
use super::config::TrimConfig;

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
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let max_lines = config.log_max_lines;

        if total <= max_lines {
            return content.to_string();
        }

        let keep_each = max_lines.saturating_sub(1) / 2;
        let keep_each = keep_each.max(3);

        let mut result: Vec<String> = Vec::new();
        result.extend(lines.iter().copied().map(|s| s.to_string()).take(keep_each));
        result.push(format!("<{} lines omitted>", total.saturating_sub(keep_each * 2)));
        result.extend(lines.iter().copied().map(|s| s.to_string()).skip(total.saturating_sub(keep_each)));

        result.join("\n")
    }
}

/// Summarizer for source code.
/// Keeps first/last N lines with a code-style ellipsis in the middle.
#[derive(Default)]
pub struct CodeSummarizer;

impl ContentSummarizer for CodeSummarizer {
    fn summarize(&self, content: &str, _budget_chars: usize, config: &TrimConfig) -> String {
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let max_lines = config.code_max_lines;

        if total <= max_lines {
            return content.to_string();
        }

        let keep_each = max_lines.saturating_sub(1) / 2;
        let keep_each = keep_each.max(5);

        let mut result: Vec<String> = Vec::new();
        result.extend(lines.iter().copied().map(|s| s.to_string()).take(keep_each));
        result.push(format!("// ... <{} lines omitted> ...", total.saturating_sub(keep_each * 2)));
        result.extend(lines.iter().copied().map(|s| s.to_string()).skip(total.saturating_sub(keep_each)));

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
        let avg_line_len = if sample_lines.is_empty() { 30 } else {
            sample_lines.iter().sum::<usize>() / sample_lines.len()
        };
        let line_cost = avg_line_len + 1; // +1 for newline
        let reserved_for_omit = 50usize; // room for "... N items omitted ..."
        let available = budget_chars.saturating_sub(reserved_for_omit);
        let budget_items = (available / line_cost).max(3);

        let max_keep = budget_items.min(max_items);
        let keep_front = (max_keep / 2).max(2);
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
#[derive(Default)]
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

    /// Trim a conversation to fit within a token budget.
    /// Uses character-count heuristic (~2 chars/token, matching estimate_tokens).
    pub fn trim_to_token_budget(
        &self,
        messages: &mut Vec<crate::types::Message>,
        target_tokens: usize,
        config: &TrimConfig,
    ) -> usize {
        if !config.is_enabled() {
            return 0;
        }

        let target_chars = target_tokens * 2;
        let system_idx = messages.iter().position(|m| m.role == "system");
        let keep_from = if let Some(idx) = system_idx { idx + 1 } else { 0 };

        if keep_from >= messages.len() {
            return 0;
        }

        let mut removed = 0;
        loop {
            let total_chars: usize = messages.iter()
                .take(messages.len().saturating_sub(1))
                .map(|m| m.content.len() + m.timestamp.len())
                .sum();

            if total_chars <= target_chars {
                break;
            }

            if keep_from >= messages.len() {
                break;
            }

            messages.remove(keep_from);
            removed += 1;
        }

        removed
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
}
