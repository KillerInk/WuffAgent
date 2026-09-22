/// Content-type classification for the trimming pipeline.
///
/// Detects what kind of content a string contains so the right
/// summarizer can be applied.

/// The detected type of content in a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentType {
    /// Build tool output (cargo, rustc, etc.).
    BuildLog,
    /// Source code (multi-line with indentation/braces).
    SourceCode,
    /// A list of files/paths (glob results, directory listings).
    FileList,
    /// Search results (web or local).
    SearchResults,
    /// An error message (starts with Error/error, has stack traces).
    ToolError,
    /// A JSON wrapper around a tool result (has result/error/entries keys).
    JsonWrapper,
    /// Free-form text (LLM response, user input).
    FreeText,
}

/// Classify the given content and return its type.
///
/// Returns the most specific type detected. Order matters:
/// more specific types are checked first.
pub fn classify_content(content: &str) -> ContentType {
    if content.is_empty() {
        return ContentType::FreeText;
    }

    let _lower = content.to_lowercase();

    // Tool errors: check first so we don't misclassify error output.
    if is_tool_error(content) {
        return ContentType::ToolError;
    }

    // Source code: check before JSON wrapper because source-code tool results
    // (e.g. file reads returning {"content":"...","total_lines":N}) must be
    // classified as SourceCode so the CodeSummarizer preserves line structure
    // instead of the generic char-based truncator mashing them into
    // "head...[...]...tail" and losing the middle content.
    if is_source_code(content) {
        return ContentType::SourceCode;
    }

    // JSON wrapper: tool results often come back as JSON with a "result" key.
    if is_json_wrapper(content) {
        return ContentType::JsonWrapper;
    }

    // Build logs: detect cargo/rustc output patterns.
    if is_build_log(content) {
        return ContentType::BuildLog;
    }

    // Search results: look for structured result arrays or "query" fields.
    if is_search_results(content) {
        return ContentType::SearchResults;
    }

    // File lists: look for path-like entries.
    if is_file_list(content) {
        return ContentType::FileList;
    }

    ContentType::FreeText
}

/// Check if content looks like a build tool output.
fn is_build_log(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 2 {
        return false;
    }

    let mut signal_count = 0;
    for line in &lines {
        let l = line.to_lowercase();
        if l.contains("compiling")
            || l.contains("finished")
            || l.contains("error[e")
            || l.contains("warning:")
            || l.contains("running")
            || l.contains("documenting")
            || l.contains("fetching")
            || l.contains("downloaded")
            || l.contains("package")
            || l.contains("fingerprint")
        {
            signal_count += 1;
        }
    }

    // At least 2 build-signal lines required.
    signal_count >= 2
}

/// Check if content is a JSON tool result wrapper.
fn is_json_wrapper(content: &str) -> bool {
    let trimmed = content.trim();
    // Quick pre-check: skip if not clearly JSON-shaped
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return false;
    }
    // Skip obviously non-JSON content (too short, contains newlines suggesting pretty-printed
    // large payloads, or clearly not a small wrapper object)
    if trimmed.len() > 4096 {
        return false;
    }

    // Must parse as JSON and have at least one of these keys.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let has_result = value.get("result").is_some();
        let has_error = value.get("error").is_some();
        let has_entries = value.get("entries").is_some();
        let has_deleted = value.get("deleted").is_some();
        let has_created = value.get("created").is_some();
        let has_bytes_written = value.get("bytes_written").is_some();
        let has_lines_changed = value.get("lines_changed").is_some();
        let has_path = value.get("path").is_some();
        let has_query = value.get("query").is_some();
        let has_results = value.get("results").is_some();

        has_result
            || has_error
            || has_entries
            || has_deleted
            || has_created
            || has_bytes_written
            || has_lines_changed
            || has_path
            || has_query
            || has_results
    } else {
        false
    }
}

/// Check if content looks like search results.
fn is_search_results(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(results) = value.get("results").and_then(|r| r.as_array()) {
                return results.len() > 1;
            }
        }
    }
    false
}

/// Check if content looks like a file/glob list.
fn is_file_list(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    // Need at least 3 lines to be a list.
    if lines.len() < 3 {
        return false;
    }

    // Count lines that look like file paths.
    let path_pattern = regex::Regex::new(r"^[a-zA-Z]:\\|^/|\.rs$|\.toml$|\.json$|\.md$|\.txt$|\.py$|\.css$|\.js$|\.ts$|\.html$|\.sh$|\.bat$").unwrap();
    let path_lines: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| path_pattern.is_match(l.trim()))
        .collect();

    // If more than half the lines look like paths, it's a file list.
    path_lines.len() as f64 > lines.len() as f64 * 0.5
}

/// Strip a `cat -n` style line-number prefix (`"    42 | content"`) if present,
/// returning the content after the `" | "` separator. File-read tool results
/// carry this prefix (when `line_numbers` is enabled), and it must not be
/// mistaken for indentation or block the brace/marker heuristics.
fn strip_line_number_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(pipe_pos) = trimmed.find(" | ") {
        let number_part = &trimmed[..pipe_pos];
        if !number_part.is_empty() && number_part.chars().all(|c| c.is_ascii_digit()) {
            return &trimmed[pipe_pos + 3..];
        }
    }
    line
}

/// Check if content looks like source code.
fn is_source_code(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 3 {
        return false;
    }

    // Count lines with significant indentation or braces.
    let code_indicators: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| {
            let l = strip_line_number_prefix(l);
            let trimmed = l.trim();
            // Lines with 4+ spaces/tabs indent, or braces, or language markers.
            l.starts_with("    ")
                || l.starts_with('\t')
                || trimmed.starts_with('{')
                || trimmed.starts_with('}')
                || trimmed.ends_with(';')
                || trimmed.contains("fn ")
                || trimmed.contains("let ")
                || trimmed.contains("if ")
                || trimmed.contains("struct ")
                || trimmed.contains("impl ")
                || trimmed.contains("pub ")
                || trimmed.contains("//")
                || trimmed.starts_with("#[")
                || trimmed.starts_with("/*")
        })
        .collect();

    code_indicators.len() as f64 > lines.len() as f64 * 0.3
}

/// Check if content is a tool error message.
fn is_tool_error(content: &str) -> bool {
    let trimmed = content.trim();
    // Starts with "Error:" or "error[E..."
    if trimmed.starts_with("Error:")
        || trimmed.starts_with("error[E")
        || trimmed.starts_with("Error ")
    {
        return true;
    }

    // Multiple lines with "error" in them and a stack trace pattern.
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 3 {
        return false;
    }

    let error_lines: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("error") && l.contains("at ")
        })
        .collect();

    error_lines.len() >= 3
}

#[cfg(test)]
mod tests;
