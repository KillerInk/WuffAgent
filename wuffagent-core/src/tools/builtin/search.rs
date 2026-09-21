//! Content search tool (`search_content`): grep/ripgrep-like pattern search
//! across files, returning matching lines with file paths and 1-based line
//! numbers. Closes the biggest gap in the named file toolset — before this,
//! searching file content required the shell tool.

use std::fs;

use crate::tools::builtin::file_io::{build_schema, required_str, validate_path};
use crate::tools::types::{Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// Default (and typical) cap on matches returned per call.
const DEFAULT_MAX_RESULTS: u64 = 100;
/// Hard cap on context lines requested per match.
const MAX_CONTEXT_LINES: u64 = 50;
/// Match lines longer than this are truncated in the output.
const MAX_LINE_LEN: usize = 500;
/// Number of leading bytes sniffed for a NUL byte (binary detection).
const BINARY_SNIFF_LEN: usize = 8000;

/// Result of scanning a single file or directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanResult {
    /// Everything was scanned (or the file was a normal text file).
    Completed,
    /// The file was skipped (binary content).
    Skipped,
    /// Scanning stopped early because the match limit was reached.
    Truncated,
}

/// A compiled line matcher (substring or regex).
type LineMatcher = Box<dyn Fn(&str) -> bool>;

struct Match {
    path: String,
    line: u64,
    text: String,
    context_before: Vec<String>,
    context_after: Vec<String>,
}

/// Build a line matcher from a pattern (literal substring or regex).
fn make_matcher(
    pattern: &str,
    is_regex: bool,
    case_sensitive: bool,
) -> Result<LineMatcher, ToolError> {
    if is_regex {
        let mut builder = regex::RegexBuilder::new(pattern);
        builder.case_insensitive(!case_sensitive);
        let re = builder.build().map_err(|e| {
            ToolError::InvalidParams(format!("Invalid regex pattern '{pattern}': {e}"))
        })?;
        Ok(Box::new(move |line: &str| re.is_match(line)))
    } else {
        let needle = if case_sensitive {
            pattern.to_string()
        } else {
            pattern.to_lowercase()
        };
        Ok(Box::new(move |line: &str| {
            if case_sensitive {
                line.contains(&needle)
            } else {
                line.to_lowercase().contains(&needle)
            }
        }))
    }
}

/// Truncate a match line to a bounded length for output.
fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_LEN {
        line.to_string()
    } else {
        let mut end = MAX_LINE_LEN;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &line[..end])
    }
}

/// Search a single text file line by line.
fn search_file(
    path: &str,
    matcher: &LineMatcher,
    context_lines: usize,
    max_results: usize,
    matches: &mut Vec<Match>,
) -> std::io::Result<ScanResult> {
    let bytes = fs::read(path)?;
    if bytes.iter().take(BINARY_SNIFF_LEN).any(|&b| b == 0) {
        return Ok(ScanResult::Skipped);
    }
    let content = String::from_utf8_lossy(&bytes);
    // Strip a UTF-8 BOM so line-anchored patterns (^needle) match on the
    // first line and the returned text is clean (consistent with read_file).
    let content = crate::tools::builtin::file_io::strip_utf8_bom(&content);
    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !matcher(line) {
            continue;
        }
        if matches.len() >= max_results {
            return Ok(ScanResult::Truncated);
        }
        let before = lines[i.saturating_sub(context_lines)..i]
            .iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        let after_end = (i + 1 + context_lines).min(lines.len());
        let after = lines[i + 1..after_end]
            .iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        matches.push(Match {
            path: path.to_string(),
            line: (i + 1) as u64,
            text: truncate_line(line.trim_end()),
            context_before: before,
            context_after: after,
        });
    }
    Ok(ScanResult::Completed)
}

/// Recursively walk a directory and search all matching text files.
/// Directory symlinks and `.git` directories are skipped.
fn walk_dir(
    dir: &str,
    matcher: &LineMatcher,
    glob_filter: Option<&glob::Pattern>,
    context_lines: usize,
    max_results: usize,
    matches: &mut Vec<Match>,
    files_searched: &mut usize,
) -> std::io::Result<ScanResult> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // file_type() does not follow symlinks.
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue; // Skip symlinks (cycle risk).
        }
        if file_type.is_dir() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name == ".git" {
                continue;
            }
            let result = walk_dir(
                entry.path().to_string_lossy().as_ref(),
                matcher,
                glob_filter,
                context_lines,
                max_results,
                matches,
                files_searched,
            )?;
            if result == ScanResult::Truncated {
                return Ok(result);
            }
        } else if file_type.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(pat) = glob_filter {
                if !pat.matches(&name) {
                    continue;
                }
            }
            let result = search_file(
                entry.path().to_string_lossy().as_ref(),
                matcher,
                context_lines,
                max_results,
                matches,
            )?;
            if result == ScanResult::Truncated {
                return Ok(result);
            }
            if result == ScanResult::Completed {
                *files_searched += 1;
            }
        }
    }
    Ok(ScanResult::Completed)
}

/// Search for a pattern in a file or directory tree (grep-like).
fn search_content(
    pattern: &str,
    path: &str,
    glob_filter: Option<&str>,
    is_regex: bool,
    case_sensitive: bool,
    context_lines: usize,
    max_results: usize,
) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(path).map_err(|e| {
        ToolError::Execution(format!("Path '{path}' not found: {e}"))
    })?;
    let matcher = make_matcher(pattern, is_regex, case_sensitive)?;

    let glob_filter = glob_filter.map(|g| {
        glob::Pattern::new(g)
            .unwrap_or_else(|_| glob::Pattern::new("*").expect("static pattern cannot fail"))
    });

    let mut matches: Vec<Match> = Vec::new();
    let mut files_searched: usize = 0;
    let result = if metadata.is_dir() {
        walk_dir(
            path,
            &matcher,
            glob_filter.as_ref(),
            context_lines,
            max_results,
            &mut matches,
            &mut files_searched,
        )?
    } else {
        match search_file(path, &matcher, context_lines, max_results, &mut matches)? {
            ScanResult::Completed => {
                files_searched = 1;
                ScanResult::Completed
            }
            other => other,
        }
    };

    Ok(ToolOutput::Success(serde_json::json!({
        "pattern": pattern,
        "path": path,
        "total_matches": matches.len(),
        "truncated": result == ScanResult::Truncated,
        "files_searched": files_searched,
        "matches": matches.iter().map(|m| {
            let mut map = serde_json::Map::new();
            map.insert("path".to_string(), serde_json::json!(m.path));
            map.insert("line".to_string(), serde_json::json!(m.line));
            map.insert("text".to_string(), serde_json::json!(m.text));
            if context_lines > 0 {
                map.insert("context_before".to_string(), serde_json::json!(m.context_before));
                map.insert("context_after".to_string(), serde_json::json!(m.context_after));
            }
            serde_json::Value::Object(map)
        }).collect::<Vec<_>>(),
    })))
}

/// Read a non-negative integer param, tolerating integral floats (e.g. `2.0`).
fn opt_uint(params: &ToolParams, key: &str) -> Option<u64> {
    params.get::<serde_json::Value>(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_f64().and_then(|f| (f >= 0.0).then_some(f as u64)))
    })
}

/// Search for a text pattern or regex in files (grep-like).
pub struct SearchContentTool;

impl SearchContentTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for SearchContentTool {
    fn name(&self) -> &str {
        "search_content"
    }
    fn description(&self) -> &str {
        "Search for a text pattern (literal substring by default, or regex) in a file or directory, like grep/ripgrep. Returns matching lines with file paths and 1-based line numbers. Skips binary files and .git directories; results are capped at max_results."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "search_content",
            "Search file contents for a pattern",
            &[
                ("pattern", "string", "Text to search for. A literal substring by default, or a regular expression when regex is true.", false),
                ("path", "string", "File or directory to search in.", false),
                ("glob", "string", "Only search files whose name matches this glob, e.g. '*.rs'.", true),
                ("regex", "boolean", "Treat pattern as a regular expression. Defaults to false (literal substring).", true),
                ("case_sensitive", "boolean", "Whether matching is case-sensitive. Defaults to true.", true),
                ("context_lines", "integer", "Lines of context before and after each match (like grep -C). Defaults to 0.", true),
                ("max_results", "integer", "Maximum number of matches to return. Defaults to 100.", true),
            ],
            &["pattern", "path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let pattern = required_str(&params, "pattern")?;
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let glob_filter: Option<String> = params.get("glob");
        let is_regex: bool = params.get("regex").unwrap_or(false);
        let case_sensitive: bool = params.get("case_sensitive").unwrap_or(true);
        let context_lines =
            opt_uint(&params, "context_lines").unwrap_or(0).min(MAX_CONTEXT_LINES) as usize;
        let max_results =
            opt_uint(&params, "max_results").unwrap_or(DEFAULT_MAX_RESULTS).max(1) as usize;
        search_content(
            &pattern,
            &path,
            glob_filter.as_deref(),
            is_regex,
            case_sensitive,
            context_lines,
            max_results,
        )
    }
}

#[cfg(test)]
mod tests;
