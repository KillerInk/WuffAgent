//! Type-specific content summarizers (D1 split from `summarizer.rs`).
//!
//! Each summarizer knows how to compress a particular content type
//! while preserving key information (paths, error messages, line counts).
use super::super::config::TrimConfig;

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
