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
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wuff_search_content_{tag}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write(p: &std::path::Path, s: &str) {
        fs::write(p, s).unwrap();
    }

    fn params(pairs: &[(&str, serde_json::Value)]) -> ToolParams {
        let mut m = std::collections::HashMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        ToolParams { values: m }
    }

    fn success_json(out: ToolOutput) -> serde_json::Value {
        match out {
            ToolOutput::Success(v) => v,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn test_search_single_file_substring() {
        let dir = temp_dir("single");
        let p = dir.join("f.txt");
        write(&p, "hello world\nfoo bar\nbaz foo\n");
        let json = success_json(
            search_content("foo", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
        assert!(!json["truncated"].as_bool().unwrap());
        let matches = json["matches"].as_array().unwrap();
        assert_eq!(matches[0]["line"].as_u64().unwrap(), 2);
        assert_eq!(matches[0]["text"].as_str().unwrap(), "foo bar");
        assert_eq!(matches[1]["line"].as_u64().unwrap(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_directory_recursive() {
        let dir = temp_dir("dir");
        let sub = dir.join("sub/deep");
        fs::create_dir_all(&sub).unwrap();
        write(&dir.join("a.txt"), "needle here\n");
        write(&sub.join("b.txt"), "no match\nneedle again\n");
        write(&sub.join("c.txt"), "nothing\n");
        let json = success_json(
            search_content("needle", dir.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
        assert_eq!(json["files_searched"].as_u64().unwrap(), 3);
        let paths: Vec<&str> = json["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("a.txt")));
        assert!(paths.iter().any(|p| p.ends_with("b.txt")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_glob_filter() {
        let dir = temp_dir("glob");
        write(&dir.join("a.txt"), "target\n");
        write(&dir.join("b.rs"), "target\n");
        write(&dir.join("c.md"), "target\n");
        let json = success_json(
            search_content(
                "target",
                dir.to_str().unwrap(),
                Some("*.txt"),
                false,
                true,
                0,
                100,
            )
            .unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
        assert!(json["matches"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("a.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_regex() {
        let dir = temp_dir("regex");
        let p = dir.join("r.txt");
        write(&p, "error 404 found\neror once\nno digits\nline 123 ok\n");
        let json = success_json(
            search_content("e+r+o+r", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
        let lines: Vec<u64> = json["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, vec![1, 2]);

        let json = success_json(
            search_content("\\d+", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
        );
        let lines: Vec<u64> = json["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, vec![1, 4]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_case_insensitive() {
        let dir = temp_dir("case");
        let p = dir.join("c.txt");
        write(&p, "Foo bar\nFOO baz\nfoo qux\n");
        // Case-sensitive by default: only exact "Foo".
        let json = success_json(
            search_content("Foo", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
        // Case-insensitive: all three.
        let json = success_json(
            search_content("foo", p.to_str().unwrap(), None, false, false, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_max_results_truncated() {
        let dir = temp_dir("maxres");
        let p = dir.join("m.txt");
        let content: Vec<String> = (1..=5).map(|i| format!("match line {i}")).collect();
        write(&p, &content.join("\n"));
        let json = success_json(
            search_content("match", p.to_str().unwrap(), None, false, true, 0, 2).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
        assert!(json["truncated"].as_bool().unwrap());
        // Exactly at the limit is not truncated.
        let json = success_json(
            search_content("match", p.to_str().unwrap(), None, false, true, 0, 5).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 5);
        assert!(!json["truncated"].as_bool().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_context_lines() {
        let dir = temp_dir("ctx");
        let p = dir.join("ctx.txt");
        write(&p, "l1\nl2\ntarget\nl4\nl5\n");
        let json = success_json(
            search_content("target", p.to_str().unwrap(), None, false, true, 1, 100).unwrap(),
        );
        let m = &json["matches"][0];
        assert_eq!(m["context_before"].as_array().unwrap(), &vec![serde_json::json!("l2")]);
        assert_eq!(m["context_after"].as_array().unwrap(), &vec![serde_json::json!("l4")]);
        // Without context lines the keys are absent.
        let json = success_json(
            search_content("target", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
        );
        assert!(json["matches"][0].get("context_before").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_binary_skipped() {
        let dir = temp_dir("binary");
        let p = dir.join("bin.dat");
        fs::write(&p, b"\x00\x01needle\x02").unwrap();
        let json = success_json(
            search_content("needle", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 0);
        assert_eq!(json["files_searched"].as_u64().unwrap(), 0, "binary files are not counted");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_git_dir_skipped() {
        let dir = temp_dir("git");
        let gitdir = dir.join(".git");
        fs::create_dir_all(&gitdir).unwrap();
        write(&gitdir.join("config"), "secret needle\n");
        write(&dir.join("a.txt"), "visible needle\n");
        let json = success_json(
            search_content("needle", dir.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
        assert!(json["matches"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("a.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_missing_path_fails() {
        let dir = temp_dir("missing");
        let err = search_content(
            "x",
            dir.join("nope").to_str().unwrap(),
            None,
            false,
            true,
            0,
            100,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_invalid_regex_fails() {
        let dir = temp_dir("badre");
        let p = dir.join("f.txt");
        write(&p, "abc\n");
        let err = search_content(
            "([unclosed",
            p.to_str().unwrap(),
            None,
            true,
            true,
            0,
            100,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_content_tool_executes() {
        let dir = temp_dir("tool");
        let p = dir.join("t.txt");
        write(&p, "alpha\nbeta alpha\ngamma\n");
        let tool = SearchContentTool::new();
        assert_eq!(tool.name(), "search_content");
        let json = success_json(
            tool.execute(params(&[
                ("pattern", serde_json::json!("alpha")),
                ("path", serde_json::json!(p.to_str().unwrap())),
            ]))
            .unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
        // Optional params via the tool: regex + case-insensitive + context.
        let json = success_json(
            tool.execute(params(&[
                ("pattern", serde_json::json!("g.m.a")),
                ("path", serde_json::json!(p.to_str().unwrap())),
                ("regex", serde_json::json!(true)),
                ("case_sensitive", serde_json::json!(false)),
                ("context_lines", serde_json::json!(1)),
            ]))
            .unwrap(),
        );
        assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
        assert!(json["matches"][0].get("context_before").is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_content_tool_requires_pattern() {
        let tool = SearchContentTool::new();
        let err = tool
            .execute(params(&[("path", serde_json::json!("."))]))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }
}
