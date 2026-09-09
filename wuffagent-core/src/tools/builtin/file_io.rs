use std::collections::HashMap;
use std::fs;
use std::io::Write;

use crate::tools::types::{FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// Validates a path for safety, rejecting traversal patterns and sensitive
/// system directories. Canonicalizes when possible for stronger guarantees.
fn validate_path(path: &str) -> Result<(), ToolError> {
    if path.is_empty() || path.len() > 4096 {
        return Err(ToolError::Execution("Path not allowed".to_string()));
    }

    let normalized = path.replace('\\', "/");
    if normalized.contains("../")
        || normalized.ends_with("/..")
        || normalized == ".."
    {
        return Err(ToolError::Execution("Path not allowed".to_string()));
    }

    let lower = normalized.to_lowercase();
    if is_sensitive_path(&lower) || lower == "c:/" {
        return Err(ToolError::Execution("Path not allowed".to_string()));
    }

    // Canonicalize when the path exists, then re-check the resolved location
    // (catches traversal via symlinks to sensitive directories).
    if let Ok(canonical) = std::path::Path::new(path).canonicalize() {
        let canonical_str = canonical.to_string_lossy().to_lowercase();
        // Strip the Windows `\\?\` verbatim prefix (exact 4-char prefix).
        let canonical_str = canonical_str.strip_prefix(r"\\?\").unwrap_or(&canonical_str);
        let canonical_norm = canonical_str.replace('\\', "/");
        if is_sensitive_path(&canonical_norm) {
            return Err(ToolError::Execution("Path not allowed".to_string()));
        }
        if let Ok(meta) = fs::metadata(&canonical) {
            if meta.file_type().is_symlink() {
                return Err(ToolError::Execution("Symlinks not allowed".to_string()));
            }
        }
    }

    Ok(())
}

fn is_sensitive_path(p: &str) -> bool {
    p == "/etc" || p.starts_with("/etc/")
        || p == "/root" || p.starts_with("/root/")
        || p == "c:/windows" || p.starts_with("c:/windows/")
        || p.starts_with("c:/program files")
}

// ─── File operation functions ───────────────────────────────────────────────

/// Read a text file, optionally limited to a line range (1-based, inclusive).
///
/// When `line_numbers` is true, each returned line is prefixed with its
/// 1-based line number in `cat -n` style (`"    42 | <content>"`).
fn read_file(
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    line_numbers: bool,
) -> crate::tools::types::ToolResult<ToolOutput> {
    use std::io::BufRead;

    // Cap the maximum number of lines returned to prevent excessive memory use.
    const MAX_LINES: usize = 10_000;

    let file = fs::File::open(path).map_err(|e| {
        ToolError::Execution(format!("Failed to open '{}': {}", path, e))
    })?;
    let reader = std::io::BufReader::new(file);

    // Params are 1-based; convert to 0-based internally.
    let start = start_line.map(|n| n - 1).unwrap_or(0);
    let end = end_line.map(|n| n - 1); // None means read until EOF
    let mut result = String::new();
    let mut line_idx = 0usize;
    let mut lines_returned = 0usize;

    for line in reader.lines() {
        let line = line.map_err(|e| {
            ToolError::Execution(format!("Failed to read line {}: {}", line_idx + 1, e))
        })?;
        // Skip lines before the start range.
        if line_idx < start {
            line_idx += 1;
            continue;
        }
        // Stop after reaching the end range (inclusive).
        if let Some(end) = end {
            if line_idx > end {
                break;
            }
        }
        let rendered = if line_numbers {
            format!("{:>6} | {}", line_idx + 1, line)
        } else {
            line
        };
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&rendered);
        lines_returned += 1;
        line_idx += 1;
        // Stop reading once we've hit the line cap.
        if lines_returned >= MAX_LINES {
            break;
        }
    }

    // Return as a JSON object with content + total_lines so the classifier
    // can recognise it as source code (not FreeText) and apply the
    // CodeSummarizer instead of the generic char-based truncator.
    Ok(ToolOutput::Success(serde_json::json!({
        "content": result,
        "total_lines": lines_returned,
        "line_numbers": line_numbers,
    })))
}

/// Write (overwrite) a text file.
fn write_file(path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    fs::write(path, content).map_err(|e| {
        ToolError::Execution(format!("Failed to write '{}': {}", path, e))
    })?;
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "bytes_written": content.len(),
        "success": true,
    })))
}

/// List entries in a directory.
fn list_dir(path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let entries: Vec<String> = fs::read_dir(path)
        .map_err(|e| {
            ToolError::Execution(format!("Failed to list '{}': {}", path, e))
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "entries": entries,
    })))
}

/// Append content to the end of a file.
fn append_file(path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            ToolError::Execution(format!("Failed to open '{}' for appending: {}", path, e))
        })?
        .write_all(content.as_bytes())
        .map_err(|e| {
            ToolError::Execution(format!("Failed to append to '{}': {}", path, e))
        })?;
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "bytes_appended": content.len(),
        "success": true,
    })))
}

/// Glob search with `*`, `?`, `[cls]`, and `**` (recursive) support.
fn search_files(pattern: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let matches: Vec<String> = glob::glob(pattern)
        .map_err(|e| {
            ToolError::Execution(format!("Invalid glob pattern '{}': {}", pattern, e))
        })?
        .filter_map(|m| m.ok())
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    Ok(ToolOutput::Success(serde_json::json!({
        "pattern": pattern,
        "matches": matches,
        "count": matches.len(),
    })))
}

/// Apply targeted edits to an existing file using SEARCH/REPLACE blocks:
///
/// ```text
/// <<<<<<< SEARCH
/// <exact text to find — must occur exactly once in the file>
/// =======
/// <replacement text (empty for pure deletion)>
/// >>>>>>> REPLACE
/// ```
///
/// Blocks are applied in order. Fails (without touching the file) if a search
/// text is not found or matches more than one place.
fn apply_diff(path: &str, diff: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let content = fs::read_to_string(path).map_err(|e| {
        ToolError::Execution(format!("Failed to read '{}' for apply_diff: {}", path, e))
    })?;

    let blocks = parse_search_replace_blocks(diff).map_err(|e| {
        ToolError::Execution(format!("Malformed search/replace blocks for '{}': {}", path, e))
    })?;
    if blocks.is_empty() {
        return Err(ToolError::Execution(
            "No search/replace blocks found in diff".to_string(),
        ));
    }

    let mut current = content;
    let mut applied: u32 = 0;
    for (i, (search, replace)) in blocks.iter().enumerate() {
        let count = current.matches(search).count();
        if count == 0 {
            return Err(ToolError::Execution(format!(
                "Block {}: search text not found in '{}'", i + 1, path
            )));
        }
        if count > 1 {
            return Err(ToolError::Execution(format!(
                "Block {}: search text matches {} places in '{}', add more context to make it unique",
                i + 1, count, path
            )));
        }
        current = current.replacen(search, replace, 1);
        applied += 1;
    }

    fs::write(path, &current).map_err(|e| {
        ToolError::Execution(format!("Failed to write patched file '{}': {}", path, e))
    })?;

    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "blocks_applied": applied,
        "success": true,
    })))
}

/// Parse SEARCH/REPLACE blocks out of a diff string.
///
/// Block delimiter lines are matched with trailing whitespace ignored; the
/// search/replace payloads themselves are taken verbatim (line by line).
fn parse_search_replace_blocks(diff: &str) -> Result<Vec<(String, String)>, String> {
    let lines: Vec<&str> = diff.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_end() != "<<<<<<< SEARCH" {
            i += 1;
            continue;
        }
        i += 1;
        let mut search = Vec::new();
        while i < lines.len() && lines[i].trim_end() != "=======" {
            search.push(lines[i]);
            i += 1;
        }
        if i >= lines.len() {
            return Err("SEARCH block missing '=======' separator".to_string());
        }
        i += 1;
        let mut replace = Vec::new();
        while i < lines.len() && lines[i].trim_end() != ">>>>>>> REPLACE" {
            replace.push(lines[i]);
            i += 1;
        }
        if i >= lines.len() {
            return Err("SEARCH block missing '>>>>>>> REPLACE' terminator".to_string());
        }
        i += 1;
        blocks.push((search.join("\n"), replace.join("\n")));
    }
    Ok(blocks)
}

// ─── Named file tools ───────────────────────────────────────────────────────
//
// Each is a thin Tool that names a single operation, so the model routes by
// clear tool names instead of picking an `action` string. They delegate to
// the free functions above.

/// Build a ToolSchema from a flat list of (name, type, description, nullable) fields.
fn build_schema(
    name: &str,
    desc: &str,
    props: &[(&str, &str, &str, bool)],
    required: &[&str],
) -> ToolSchema {
    let mut map = HashMap::new();
    for (k, t, d, n) in props {
        map.insert(
            k.to_string(),
            FieldSchema {
                type_name: t.to_string(),
                description: d.to_string(),
                nullable: *n,
            },
        );
    }
    ToolSchema {
        name: name.to_string(),
        description: desc.to_string(),
        input_type: Some(JsonSchema {
            type_name: "object".to_string(),
            properties: Some(map),
            required: required.iter().map(|s| s.to_string()).collect(),
        }),
    }
}

fn required_str(params: &ToolParams, key: &str) -> Result<String, ToolError> {
    params
        .get(key)
        .ok_or_else(|| ToolError::InvalidParams(format!("{key} is required")))
}

/// Read a text file, optionally limited to a line range.
pub struct ReadFileTool;

impl ReadFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a text file, optionally limited to a line range. Returns the file content."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "read_file",
            "Read a text file, optionally limited to a line range",
            &[
                ("path", "string", "Path of the file to read.", false),
                ("start_line", "integer", "First line to read (1-indexed). Defaults to 1.", true),
                ("end_line", "integer", "Last line to read (1-indexed, inclusive). Defaults to end of file.", true),
                ("line_numbers", "boolean", "Prefix each line with its 1-based line number. Defaults to true.", true),
            ],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let start_line: Option<usize> = params.get("start_line");
        let end_line: Option<usize> = params.get("end_line");
        let line_numbers: bool = params.get("line_numbers").unwrap_or(true);
        read_file(&path, start_line, end_line, line_numbers)
    }
}

/// Write (overwrite) a text file.
pub struct WriteFileTool;

impl WriteFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write (overwrite) a text file. The full content must be provided."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "write_file",
            "Write (overwrite) a text file",
            &[
                ("path", "string", "Path of the file to write.", false),
                ("content", "string", "Full content to write to the file.", false),
            ],
            &["path", "content"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let content = required_str(&params, "content")?;
        write_file(&path, &content)
    }
}

/// Append content to the end of a file.
pub struct AppendFileTool;

impl AppendFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for AppendFileTool {
    fn name(&self) -> &str {
        "append_file"
    }
    fn description(&self) -> &str {
        "Append content to the end of a file (creates the file if it does not exist)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "append_file",
            "Append content to the end of a file",
            &[
                ("path", "string", "Path of the file to append to.", false),
                ("content", "string", "Content to append.", false),
            ],
            &["path", "content"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let content = required_str(&params, "content")?;
        append_file(&path, &content)
    }
}

/// List entries in a directory.
pub struct ListDirTool;

impl ListDirTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List the entries (files and directories) inside a directory."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "list_dir",
            "List the entries inside a directory",
            &[("path", "string", "Path of the directory to list.", false)],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        list_dir(&path)
    }
}

/// Glob search for files.
pub struct SearchFilesTool;

impl SearchFilesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }
    fn description(&self) -> &str {
        "Find files matching a glob pattern (supports *, ?, [cls], and ** recursive)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "search_files",
            "Find files matching a glob pattern",
            &[("pattern", "string", "Glob pattern to match files against.", false)],
            &["pattern"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let pattern = required_str(&params, "pattern")?;
        search_files(&pattern)
    }
}

/// Apply targeted edits to a file via SEARCH/REPLACE blocks.
pub struct ApplyDiffTool;

impl ApplyDiffTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ApplyDiffTool {
    fn name(&self) -> &str {
        "apply_diff"
    }
    fn description(&self) -> &str {
        "Apply targeted edits to an existing file using SEARCH/REPLACE blocks. \
         Each block's SEARCH text must match exactly one place in the file. \
         Use read_file first to copy the exact current text."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "apply_diff",
            "Apply targeted edits to an existing file using SEARCH/REPLACE blocks",
            &[
                ("path", "string", "Path of the file to edit.", false),
                ("diff", "string",
                 "One or more SEARCH/REPLACE blocks in the form:\n\
                  <<<<<<< SEARCH\n<exact text to find (must be unique)>\n=======\n<replacement text>\n>>>>>>> REPLACE\n\
                  The SEARCH text must match exactly once in the file; provide enough surrounding context to be unique.",
                 false),
            ],
            &["path", "diff"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let diff = required_str(&params, "diff")?;
        apply_diff(&path, &diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wuff_file_io_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write(p: &str, s: &str) {
        fs::write(p, s).unwrap();
    }

    fn read_string(path: &str) -> String {
        let mut buf = String::new();
        let mut file = fs::File::open(path).unwrap();
        file.read_to_string(&mut buf).unwrap();
        buf
    }

    fn params(pairs: &[(&str, &str)]) -> ToolParams {
        let mut m = HashMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), serde_json::json!(v));
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
    fn test_read_file_full() {
        let dir = temp_dir("read");
        let p = dir.join("a.txt");
        write(p.to_str().unwrap(), "line1\nline2\nline3\n");
        let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
        let json = success_json(out);
        let content = json["content"].as_str().unwrap();
        assert!(content.contains("line1") && content.contains("line3"));
        assert_eq!(json["total_lines"].as_u64().unwrap(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_file_line_range() {
        let dir = temp_dir("range");
        let p = dir.join("r.txt");
        write(p.to_str().unwrap(), "a\nb\nc\nd\n");
        let out = read_file(p.to_str().unwrap(), Some(2), Some(3), false).unwrap();
        let json = success_json(out);
        assert_eq!(json["content"].as_str().unwrap(), "b\nc");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_file_line_numbers() {
        let dir = temp_dir("nums");
        let p = dir.join("n.txt");
        write(p.to_str().unwrap(), "hello\n");
        let out = read_file(p.to_str().unwrap(), None, None, true).unwrap();
        let json = success_json(out);
        let content = json["content"].as_str().unwrap();
        assert!(content.contains("1 | hello"), "got: {content}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        let dir = temp_dir("rt");
        let p = dir.join("rt.txt");
        write_file(p.to_str().unwrap(), "data\nmore\n").unwrap();
        let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
        let json = success_json(out);
        assert_eq!(json["content"].as_str().unwrap(), "data\nmore");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_append_file() {
        let dir = temp_dir("app");
        let p = dir.join("app.txt");
        write_file(p.to_str().unwrap(), "one\n").unwrap();
        append_file(p.to_str().unwrap(), "two\n").unwrap();
        // read_file is line-based, so compare the raw file bytes for the
        // exact round-trip (including the trailing newline).
        assert_eq!(read_string(p.to_str().unwrap()), "one\ntwo\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_dir() {
        let dir = temp_dir("ls");
        let p = dir.join("x.txt");
        write(p.to_str().unwrap(), "x");
        let out = list_dir(dir.to_str().unwrap()).unwrap();
        let json = success_json(out);
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_files_glob() {
        let dir = temp_dir("glob");
        let p = dir.join("match.txt");
        write(p.to_str().unwrap(), "m");
        let pattern = dir.join("*").to_string_lossy().to_string();
        let out = search_files(&pattern).unwrap();
        let json = success_json(out);
        assert_eq!(json["count"].as_u64().unwrap(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_path_rejects_sensitive() {
        assert!(validate_path("C:/Windows").is_err());
        assert!(validate_path("/etc/passwd").is_err());
        assert!(validate_path("../escape").is_err());
        assert!(validate_path("").is_err());
    }

    #[test]
    fn test_validate_path_allows_normal() {
        let dir = temp_dir("ok");
        let p = dir.join("file.txt");
        write(p.to_str().unwrap(), "ok");
        assert!(validate_path(p.to_str().unwrap()).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_single_block() {
        let dir = temp_dir("diff1");
        let p = dir.join("d.txt");
        write(p.to_str().unwrap(), "fn main() {\n    println!(\"old\");\n}\n");
        let diff = "<<<<<<< SEARCH\n    println!(\"old\");\n=======\n    println!(\"new\");\n>>>>>>> REPLACE\n";
        let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
        let json = success_json(out);
        assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
        let content = read_string(p.to_str().unwrap());
        assert!(content.contains("\"new\"") && !content.contains("\"old\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_multi_block() {
        let dir = temp_dir("diffm");
        let p = dir.join("m.txt");
        write(p.to_str().unwrap(), "alpha\nbeta\ngamma\n");
        let diff = "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n<<<<<<< SEARCH\ngamma\n=======\nGAMMA\n>>>>>>> REPLACE\n";
        let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
        let json = success_json(out);
        assert_eq!(json["blocks_applied"].as_u64().unwrap(), 2);
        assert_eq!(read_string(p.to_str().unwrap()), "ALPHA\nbeta\nGAMMA\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_delete_only() {
        let dir = temp_dir("diffdel");
        let p = dir.join("del.txt");
        write(p.to_str().unwrap(), "keep\ngone\nkeep2\n");
        let diff = "<<<<<<< SEARCH\ngone\n\n=======\n>>>>>>> REPLACE\n";
        apply_diff(p.to_str().unwrap(), diff).unwrap();
        assert_eq!(read_string(p.to_str().unwrap()), "keep\nkeep2\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_not_found_fails() {
        let dir = temp_dir("diffnf");
        let p = dir.join("nf.txt");
        let original = "unchanged\n";
        write(p.to_str().unwrap(), original);
        let diff = "<<<<<<< SEARCH\nmissing\n=======\nx\n>>>>>>> REPLACE\n";
        let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
        let ToolError::Execution(msg) = &err else { panic!("{err:?}") };
        assert!(msg.contains("not found"), "msg: {msg}");
        assert_eq!(read_string(p.to_str().unwrap()), original);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_ambiguous_fails() {
        let dir = temp_dir("diffamb");
        let p = dir.join("amb.txt");
        let original = "dup\ndup\ndup\n";
        write(p.to_str().unwrap(), original);
        let diff = "<<<<<<< SEARCH\ndup\n=======\nunique\n>>>>>>> REPLACE\n";
        let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
        let ToolError::Execution(msg) = &err else { panic!("{err:?}") };
        assert!(msg.contains("3 places"), "msg: {msg}");
        assert_eq!(read_string(p.to_str().unwrap()), original);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_malformed_fails() {
        let dir = temp_dir("diffmal");
        let p = dir.join("mal.txt");
        let original = "content\n";
        write(p.to_str().unwrap(), original);
        let err = apply_diff(p.to_str().unwrap(), "just some text").unwrap_err();
        let ToolError::Execution(msg) = &err else { panic!("{err:?}") };
        assert!(msg.contains("No search/replace blocks"), "msg: {msg}");
        assert_eq!(read_string(p.to_str().unwrap()), original);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_preserves_crlf() {
        let dir = temp_dir("diffcrlf");
        let p = dir.join("w.txt");
        fs::write(&p, "hello\r\nworld\r\n").unwrap();
        let diff = "<<<<<<< SEARCH\nhello\n=======\nHELLO\n>>>>>>> REPLACE\n";
        apply_diff(p.to_str().unwrap(), diff).unwrap();
        let bytes = fs::read(p.to_str().unwrap()).unwrap();
        assert!(bytes.windows(2).any(|w| w == b"\r\n"), "CRLF not preserved");
        assert!(bytes.starts_with(b"HELLO"), "replacement not applied");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_context_anchoring() {
        let dir = temp_dir("diffctx");
        let p = dir.join("ctx.txt");
        write(
            p.to_str().unwrap(),
            "fn foo() {\n    let x = 1;\n    let y = 2;\n}\nfn foo() {\n    let x = 1;\n    let y = 3;\n}\n",
        );
        let diff = "<<<<<<< SEARCH\n    let y = 3;\n=======\n    let y = 30;\n>>>>>>> REPLACE\n";
        apply_diff(p.to_str().unwrap(), diff).unwrap();
        let content = read_string(p.to_str().unwrap());
        assert!(content.contains("let y = 30;"), "content: {content}");
        assert!(content.contains("let y = 2;"), "content: {content}");
        let _ = fs::remove_dir_all(&dir);
    }

    // ─── Named tool wiring tests ───────────────────────────────────────────

    #[test]
    fn test_read_file_tool_executes() {
        let dir = temp_dir("toolread");
        let p = dir.join("t.txt");
        write(p.to_str().unwrap(), "abc\n");
        let tool = ReadFileTool::new();
        assert_eq!(tool.name(), "read_file");
        let mut prms = params(&[("path", p.to_str().unwrap())]);
        prms.values.insert("line_numbers".to_string(), serde_json::json!(false));
        let out = tool.execute(prms).unwrap();
        let json = success_json(out);
        assert_eq!(json["content"].as_str().unwrap(), "abc");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_file_tool_requires_content() {
        let tool = WriteFileTool::new();
        let err = tool.execute(params(&[("path", "x.txt")])).unwrap_err();
        let ToolError::InvalidParams(msg) = &err else { panic!("{err:?}") };
        assert!(msg.contains("content"));
    }

    #[test]
    fn test_apply_diff_tool_executes() {
        let dir = temp_dir("tooldiff");
        let p = dir.join("td.txt");
        write(p.to_str().unwrap(), "before\n");
        let tool = ApplyDiffTool::new();
        assert_eq!(tool.name(), "apply_diff");
        let diff = "<<<<<<< SEARCH\nbefore\n=======\nafter\n>>>>>>> REPLACE\n";
        let out = tool
            .execute(params(&[("path", p.to_str().unwrap()), ("diff", diff)]))
            .unwrap();
        let json = success_json(out);
        assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tool_names_unique() {
        let read = ReadFileTool::new();
        let write = WriteFileTool::new();
        let append = AppendFileTool::new();
        let list = ListDirTool::new();
        let search = SearchFilesTool::new();
        let diff = ApplyDiffTool::new();
        let names = vec![
            read.name(),
            write.name(),
            append.name(),
            list.name(),
            search.name(),
            diff.name(),
        ];
        let set: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(set.len(), 6, "tool names must be unique: {names:?}");
    }
}
