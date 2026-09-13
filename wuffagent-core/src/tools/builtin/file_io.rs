use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::tools::types::{FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// Validates a path for safety, rejecting traversal patterns and sensitive
/// system directories. Canonicalizes when possible for stronger guarantees.
pub(crate) fn validate_path(path: &str) -> Result<(), ToolError> {
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
///
/// Content is capped (total bytes, line count, and per-line length) to
/// protect the model's context window. When a cap cuts the content, the
/// result carries `truncated: true` so the model can page with
/// `start_line`/`end_line`. `total_lines` always reports the full file
/// line count, so the model knows how far the file continues.
fn read_file(
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    line_numbers: bool,
) -> crate::tools::types::ToolResult<ToolOutput> {
    use std::io::BufRead;

    /// Hard cap on the number of lines returned.
    const MAX_LINES: usize = 10_000;
    /// Hard cap on the size of returned content (~256 KB).
    const MAX_CONTENT_BYTES: usize = 256 * 1024;
    /// Individual lines longer than this are trimmed.
    const MAX_LINE_LEN: usize = 10_000;

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
    let mut total_lines = 0usize;
    let mut truncated = false;

    for line in reader.lines() {
        let line = line.map_err(|e| {
            ToolError::Execution(format!("Failed to read line {}: {}", line_idx + 1, e))
        })?;
        // Keep counting every line so total_lines reflects the whole file,
        // even past the requested range or the content caps.
        total_lines += 1;

        let in_range = line_idx >= start
            && match end {
                Some(e) => line_idx <= e,
                None => true,
            };
        if !in_range {
            line_idx += 1;
            continue;
        }

        if lines_returned >= MAX_LINES || result.len() >= MAX_CONTENT_BYTES {
            truncated = true;
            line_idx += 1;
            continue;
        }

        // Trim oversized individual lines (minified code, long log lines)
        // so one huge line cannot blow up the output.
        let line: String = if line.len() > MAX_LINE_LEN {
            let mut cut = MAX_LINE_LEN;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &line[..cut])
        } else {
            line
        };

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
    }

    // Return as a JSON object with content + total_lines so the classifier
    // can recognise it as source code (not FreeText) and apply the
    // CodeSummarizer instead of the generic char-based truncator.
    Ok(ToolOutput::Success(serde_json::json!({
        "content": result,
        "total_lines": total_lines,
        "lines_returned": lines_returned,
        "truncated": truncated,
        "line_numbers": line_numbers,
    })))
}

/// Rename `tmp` over `target`, replacing an existing file.
///
/// On Unix `fs::rename` replaces atomically. On Windows `fs::rename`
/// refuses to overwrite an existing target, so we remove it first and
/// retry — the target may briefly not exist, but it is never
/// half-written, which is the invariant that matters here.
fn replace_over_existing(tmp: &Path, target: &Path) -> std::io::Result<()> {
    match fs::rename(tmp, target) {
        Ok(()) => Ok(()),
        // Windows reports AlreadyExists when the target exists.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(target)?;
            fs::rename(tmp, target)
        }
        Err(e) => Err(e),
    }
}

/// Write (overwrite) a text file.
///
/// Parent directories are created if they do not exist. The write is
/// atomic: content is first fully written (and synced) to a temporary
/// file in the same directory, then renamed over the target — a crash
/// mid-write never leaves a truncated file at the target path.
fn write_file(path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let target = Path::new(path);
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                ToolError::Execution(format!(
                    "Failed to create parent directory for '{}': {}",
                    path, e
                ))
            })?;
        }
    }

    // Temp file next to the target (same volume keeps the rename atomic).
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let tmp = target.with_file_name(format!("{}.tmp.{}-{}", file_name, std::process::id(), n));

    let write_result: std::io::Result<()> = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_data()
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(ToolError::Execution(format!("Failed to write '{}': {}", path, e)));
    }
    if let Err(e) = replace_over_existing(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(ToolError::Execution(format!("Failed to replace '{}': {}", path, e)));
    }
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "bytes_written": content.len(),
        "success": true,
    })))
}

/// List directory entries with per-entry metadata: `type` (dir/file/
/// symlink/other) and `size` in bytes for regular files. Directories are
/// listed first, then files, each sorted by name — the OS enumeration
/// order is otherwise arbitrary.
fn list_dir(path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let dir_iter = fs::read_dir(path).map_err(|e| {
        ToolError::Execution(format!("Failed to list '{}': {}", path, e))
    })?;
    let mut entries: Vec<(bool, String, serde_json::Value)> = Vec::new();
    for e in dir_iter {
        let e = match e {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = e.file_name().to_string_lossy().to_string();
        // file_type() does not follow symlinks.
        let (is_dir, type_str, size) = match e.file_type() {
            Ok(t) if t.is_symlink() => (false, "symlink", None),
            Ok(t) if t.is_dir() => (true, "dir", None),
            Ok(t) if t.is_file() => (false, "file", e.metadata().ok().map(|m| m.len())),
            _ => (false, "other", None),
        };
        let mut value = serde_json::Map::new();
        value.insert("name".to_string(), serde_json::json!(name.clone()));
        value.insert("type".to_string(), serde_json::json!(type_str));
        if let Some(s) = size {
            value.insert("size".to_string(), serde_json::json!(s));
        }
        entries.push((is_dir, name, serde_json::Value::Object(value)));
    }
    // Directories first, then by name.
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let list: Vec<serde_json::Value> = entries.into_iter().map(|(_, _, v)| v).collect();
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "entries": list,
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
///
/// Matches under `.git` or `target` directories are skipped (VCS metadata
/// and build artifacts are noise), and returned matches are capped
/// (`max_results`, default 500) with a `truncated` flag when the cap stops
/// the walk early.
fn search_files(
    pattern: &str,
    max_results: Option<usize>,
) -> crate::tools::types::ToolResult<ToolOutput> {
    /// Default cap on matches returned (keeps output usable and encourages
    /// narrower patterns for huge searches).
    const DEFAULT_MAX_RESULTS: usize = 500;
    /// Directory components always skipped.
    const SKIP_DIRS: &[&str] = &[".git", "target"];

    let cap = max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);
    let mut iter = glob::glob(pattern).map_err(|e| {
        ToolError::Execution(format!("Invalid glob pattern '{}': {}", pattern, e))
    })?;

    let mut matches: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut truncated = false;
    while let Some(m) = iter.next() {
        let p = match m {
            Ok(p) => p,
            Err(_) => continue,
        };
        if p.components().any(|c| {
            matches!(
                c,
                std::path::Component::Normal(n)
                    if SKIP_DIRS.contains(&n.to_str().unwrap_or(""))
            )
        }) {
            skipped += 1;
            continue;
        }
        matches.push(p.to_string_lossy().to_string());
        if matches.len() >= cap {
            truncated = iter.next().is_some();
            break;
        }
    }
    Ok(ToolOutput::Success(serde_json::json!({
        "pattern": pattern,
        "matches": matches,
        "count": matches.len(),
        "skipped": skipped,
        "truncated": truncated,
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
///
/// Line endings: SEARCH/REPLACE payloads are matched against a
/// line-ending-normalized copy of the file (SEARCH text is always
/// LF-normalized, but files on Windows are typically CRLF). The file's
/// original line ending is restored before writing, so editing a CRLF file
/// never rewrites it as LF.
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

    // Match on a line-ending-normalized copy (see function docs).
    let file_is_crlf = content.contains("\r\n");
    let mut current: String = if file_is_crlf {
        content.replace("\r\n", "\n")
    } else {
        content
    };

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

    if file_is_crlf {
        current = current.replace('\n', "\r\n");
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

/// Create a directory. If `recursive` is true, parent directories are
/// created as needed (`mkdir -p` semantics).
fn mkdir(path: &str, recursive: bool) -> crate::tools::types::ToolResult<ToolOutput> {
    let result = if recursive {
        fs::create_dir_all(path)
    } else {
        fs::create_dir(path)
    };
    result.map_err(|e| {
        ToolError::Execution(format!("Failed to create directory '{}': {}", path, e))
    })?;
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "created": true,
    })))
}

/// Delete a file or directory. If `recursive` is true, directories and all
/// their contents are deleted; otherwise a directory must be empty.
fn delete(path: &str, recursive: bool) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(path).map_err(|e| {
        ToolError::Execution(format!("Failed to stat '{}': {}", path, e))
    })?;
    if metadata.is_dir() {
        if recursive {
            fs::remove_dir_all(path).map_err(|e| {
                ToolError::Execution(format!("Failed to remove directory '{}': {}", path, e))
            })?;
        } else {
            fs::remove_dir(path).map_err(|e| {
                ToolError::Execution(format!(
                    "Failed to remove directory '{}': {} (directory may not be empty; use recursive: true to delete non-empty directories)",
                    path, e
                ))
            })?;
        }
    } else {
        fs::remove_file(path).map_err(|e| {
            ToolError::Execution(format!("Failed to remove file '{}': {}", path, e))
        })?;
    }
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "deleted": true,
    })))
}

/// Copy a file or directory (directories are copied recursively).
fn copy(src: &str, dest: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(src).map_err(|e| {
        ToolError::Execution(format!("Source '{}' not found: {}", src, e))
    })?;
    if metadata.is_dir() {
        copy_dir_all(src, dest).map_err(|e| {
            ToolError::Execution(format!("Failed to copy directory '{}': {}", src, e))
        })?;
    } else {
        fs::copy(src, dest).map_err(|e| {
            ToolError::Execution(format!("Failed to copy '{}' to '{}': {}", src, dest, e))
        })?;
    }
    Ok(ToolOutput::Success(serde_json::json!({
        "src": src,
        "dest": dest,
        "copied": true,
    })))
}

/// Recursively copy a directory, skipping symlinks and tracking visited
/// paths to prevent infinite recursion from cycles.
fn copy_dir_all(src: &str, dest: &str) -> std::io::Result<()> {
    copy_dir_all_inner(src, dest, &mut std::collections::HashSet::new())
}

fn copy_dir_all_inner(
    src: &str,
    dest: &str,
    visited: &mut std::collections::HashSet<String>,
) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = std::path::Path::new(dest).join(entry.file_name());

        // Skip symlinks to prevent infinite recursion and symlink attacks.
        if src_path.symlink_metadata()?.file_type().is_symlink() {
            continue;
        }

        if src_path.is_dir() {
            // Prevent cycles by tracking visited paths.
            let key = src_path.to_string_lossy().to_string();
            if !visited.insert(key) {
                continue; // Cycle detected, skip.
            }
            copy_dir_all_inner(
                src_path.to_string_lossy().as_ref(),
                dest_path.to_string_lossy().as_ref(),
                visited,
            )?;
        } else {
            fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

/// Move or rename a file or directory. Falls back to copy + delete for
/// cross-device moves.
fn move_item(src: &str, dest: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    if fs::metadata(src).is_err() {
        return Err(ToolError::Execution(format!("Source '{}' not found", src)));
    }
    if fs::rename(src, dest).is_err() {
        // Try cross-device fallback
        copy(src, dest)?;
        fs::remove_file(src).or_else(|_| fs::remove_dir_all(src)).map_err(|e| {
            ToolError::Execution(format!(
                "Cross-device move failed after copy: {}", e
            ))
        })?;
        return Ok(ToolOutput::Success(serde_json::json!({
            "src": src,
            "dest": dest,
            "moved": true,
            "cross_device": true,
        })));
    }
    Ok(ToolOutput::Success(serde_json::json!({
        "src": src,
        "dest": dest,
        "moved": true,
    })))
}

/// Stat a file or directory: size, last-modified time, type, and permissions.
fn file_info(path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(path).map_err(|e| {
        ToolError::Execution(format!("Failed to stat '{}': {}", path, e))
    })?;
    let perms = metadata.permissions();
    #[cfg(unix)]
    let perm_str = {
        use std::os::unix::fs::PermissionsExt;
        format!("{:04o}", perms.mode())
    };
    #[cfg(windows)]
    let perm_str = {
        let mut s = String::new();
        if perms.readonly() {
            s.push_str("readonly");
        } else {
            s.push_str("rw");
        }
        s
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "size": metadata.len(),
        "mtime": mtime,
        "is_dir": metadata.is_dir(),
        "is_file": metadata.is_file(),
        "permissions": perm_str,
    })))
}

// ─── Named file tools ───────────────────────────────────────────────────────
//
// Each is a thin Tool that names a single operation, so the model routes by
// clear tool names instead of picking an `action` string. They delegate to
// the free functions above.

/// Build a ToolSchema from a flat list of (name, type, description, nullable) fields.
pub(crate) fn build_schema(
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

pub(crate) fn required_str(params: &ToolParams, key: &str) -> Result<String, ToolError> {
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
        "Read a text file, optionally limited to a line range. Returns the file content; large files are truncated with a `truncated` flag and `total_lines` for paging."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "read_file",
            "Read a text file, optionally limited to a line range",
            &[
                ("path", "string", "Path of the file to read.", false),
                ("start_line", "integer", "First line to read (1-indexed). Defaults to 1.", true),
                ("end_line", "integer", "Last line to read (1-indexed, inclusive). Defaults to end of file. Use with total_lines to page through large files.", true),
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
        "Write (overwrite) a text file. The full content must be provided. Parent directories are created if missing; the write is atomic (temp file + rename), so a failure never leaves a truncated file."
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
        "List the entries (files and directories) inside a directory with type and file size; directories are listed first."
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
        "Find files matching a glob pattern (supports *, ?, [cls], and ** recursive). Skips .git and target directories; results are capped (default 500)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "search_files",
            "Find files matching a glob pattern",
            &[
                ("pattern", "string", "Glob pattern to match files against.", false),
                ("max_results", "integer", "Maximum number to return. Defaults to 500.", true),
            ],
            &["pattern"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let pattern = required_str(&params, "pattern")?;
        let max_results: Option<usize> = params.get("max_results");
        search_files(&pattern, max_results)
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

/// Create a directory.
pub struct MkdirTool;

impl MkdirTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for MkdirTool {
    fn name(&self) -> &str {
        "mkdir"
    }
    fn description(&self) -> &str {
        "Create a directory (parent directories are created as needed when recursive is true)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "mkdir",
            "Create a directory",
            &[
                ("path", "string", "Path of the directory to create.", false),
                ("recursive", "boolean", "Create parent directories as needed. Defaults to true.", true),
            ],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let recursive: bool = params.get("recursive").unwrap_or(true);
        mkdir(&path, recursive)
    }
}

/// Delete a file or directory.
pub struct DeleteTool;

impl DeleteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for DeleteTool {
    fn name(&self) -> &str {
        "delete"
    }
    fn description(&self) -> &str {
        "Delete a file or directory. Deleting a non-empty directory requires recursive: true."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "delete",
            "Delete a file or directory",
            &[
                ("path", "string", "Path of the file or directory to delete.", false),
                ("recursive", "boolean", "Delete directories recursively with all their contents. Defaults to false.", true),
            ],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let recursive: bool = params.get("recursive").unwrap_or(false);
        delete(&path, recursive)
    }
}

/// Copy a file or directory.
pub struct CopyTool;

impl CopyTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for CopyTool {
    fn name(&self) -> &str {
        "copy"
    }
    fn description(&self) -> &str {
        "Copy a file or directory (directories are copied recursively)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "copy",
            "Copy a file or directory",
            &[
                ("src", "string", "Path of the source file or directory.", false),
                ("dest", "string", "Path of the destination file or directory.", false),
            ],
            &["src", "dest"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let src = required_str(&params, "src")?;
        if let Err(e) = validate_path(&src) {
            return Err(e);
        }
        let dest = required_str(&params, "dest")?;
        if let Err(e) = validate_path(&dest) {
            return Err(e);
        }
        copy(&src, &dest)
    }
}

/// Move or rename a file or directory.
pub struct MoveTool;

impl MoveTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for MoveTool {
    fn name(&self) -> &str {
        "move"
    }
    fn description(&self) -> &str {
        "Move or rename a file or directory."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "move",
            "Move or rename a file or directory",
            &[
                ("src", "string", "Current path of the file or directory.", false),
                ("dest", "string", "New path for the file or directory.", false),
            ],
            &["src", "dest"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let src = required_str(&params, "src")?;
        if let Err(e) = validate_path(&src) {
            return Err(e);
        }
        let dest = required_str(&params, "dest")?;
        if let Err(e) = validate_path(&dest) {
            return Err(e);
        }
        move_item(&src, &dest)
    }
}

/// Get file or directory metadata.
pub struct FileInfoTool;

impl FileInfoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for FileInfoTool {
    fn name(&self) -> &str {
        "file_info"
    }
    fn description(&self) -> &str {
        "Get file or directory metadata: size, last-modified time, type, and permissions."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "file_info",
            "Get file or directory metadata",
            &[(
                "path",
                "string",
                "Path of the file or directory to inspect.",
                false,
            )],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        file_info(&path)
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
    fn test_read_file_truncates_large_file() {
        let dir = temp_dir("bigread");
        let p = dir.join("big.txt");
        // 1200 lines x 300 bytes = 360 KB > 256 KB cap.
        let line = "x".repeat(299) + "\n";
        write(p.to_str().unwrap(), &line.repeat(1200));
        let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
        let json = success_json(out);
        assert_eq!(json["total_lines"].as_u64().unwrap(), 1200);
        assert!(json["truncated"].as_bool().unwrap());
        assert!(json["lines_returned"].as_u64().unwrap() < 1200);
        let content = json["content"].as_str().unwrap();
        assert!(
            content.len() < 256 * 1024 + 310,
            "content exceeds cap: {}",
            content.len()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_file_total_lines_with_range() {
        let dir = temp_dir("rangetotal");
        let p = dir.join("rt.txt");
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        write(p.to_str().unwrap(), &content);
        let out = read_file(p.to_str().unwrap(), Some(10), Some(20), false).unwrap();
        let json = success_json(out);
        assert_eq!(json["total_lines"].as_u64().unwrap(), 100);
        assert_eq!(json["lines_returned"].as_u64().unwrap(), 11);
        assert!(!json["truncated"].as_bool().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_file_long_line_trimmed() {
        let dir = temp_dir("longline");
        let p = dir.join("long.txt");
        write(p.to_str().unwrap(), &format!("short\n{}\ntail\n", "y".repeat(50_000)));
        let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
        let json = success_json(out);
        assert!(!json["truncated"].as_bool().unwrap());
        let lines: Vec<&str> = json["content"].as_str().unwrap().lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].ends_with('…'));
        assert!(lines[1].len() < 10_100);
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
        assert_eq!(entries[0]["name"].as_str().unwrap(), "x.txt");
        assert_eq!(entries[0]["type"].as_str().unwrap(), "file");
        assert_eq!(entries[0]["size"].as_u64().unwrap(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_dir_dirs_first_and_symlinks() {
        let dir = temp_dir("lsorder");
        write(dir.join("b.txt").to_str().unwrap(), "bb");
        write(dir.join("a.txt").to_str().unwrap(), "aaaaa");
        fs::create_dir_all(dir.join("sub")).unwrap();
        // Best-effort symlink (requires privileges on Windows).
        let link = dir.join("link.txt");
        let made = {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(dir.join("a.txt"), &link).is_ok()
            }
            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_file(dir.join("a.txt"), &link).is_ok()
            }
        };
        let out = list_dir(dir.to_str().unwrap()).unwrap();
        let json = success_json(out);
        let entries = json["entries"].as_array().unwrap();
        // The directory must come first.
        assert_eq!(entries[0]["name"].as_str().unwrap(), "sub");
        assert_eq!(entries[0]["type"].as_str().unwrap(), "dir");
        assert!(entries[0].get("size").is_none());
        // Files follow, sorted by name, with sizes.
        let names: Vec<&str> = entries[1..].iter().map(|e| e["name"].as_str().unwrap()).collect();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(names, sorted_names);
        let a = entries.iter().find(|e| e["name"] == "a.txt").unwrap();
        assert_eq!(a["size"].as_u64().unwrap(), 5);
        if made {
            let l = entries.iter().find(|e| e["name"] == "link.txt").unwrap();
            assert_eq!(l["type"].as_str().unwrap(), "symlink");
            assert!(l.get("size").is_none());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_file_creates_parent_dirs() {
        let dir = temp_dir("parent");
        let p = dir.join("a/b/c/deep.txt");
        let out = write_file(p.to_str().unwrap(), "nested").unwrap();
        let json = success_json(out);
        assert_eq!(json["bytes_written"].as_u64().unwrap(), 6);
        assert_eq!(read_string(p.to_str().unwrap()), "nested");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_file_replaces_existing_without_temp_leftovers() {
        let dir = temp_dir("atomic");
        let p = dir.join("f.txt");
        write_file(p.to_str().unwrap(), "version one\n").unwrap();
        write_file(p.to_str().unwrap(), "version two\n").unwrap();
        assert_eq!(read_string(p.to_str().unwrap()), "version two\n");
        // No .tmp. files may remain in the directory.
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_files_glob() {
        let dir = temp_dir("glob");
        let p = dir.join("match.txt");
        write(p.to_str().unwrap(), "m");
        let pattern = dir.join("*").to_string_lossy().to_string();
        let out = search_files(&pattern, None).unwrap();
        let json = success_json(out);
        assert_eq!(json["count"].as_u64().unwrap(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_files_skips_git_and_target() {
        let dir = temp_dir("skipdirs");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        write(dir.join("a.txt").to_str().unwrap(), "a");
        write(dir.join(".git/config").to_str().unwrap(), "c");
        write(dir.join("target/debug/b.txt").to_str().unwrap(), "b");
        // Single level: .git and target themselves are filtered out.
        let pattern = dir.join("*").to_string_lossy().to_string();
        let out = search_files(&pattern, None).unwrap();
        let json = success_json(out);
        assert_eq!(json["count"].as_u64().unwrap(), 1);
        assert_eq!(json["skipped"].as_u64().unwrap(), 2);
        assert!(json["matches"][0]
            .as_str()
            .unwrap()
            .ends_with("a.txt"));
        // Recursive: nothing under .git or target may appear.
        let pattern2 = dir.join("**").to_string_lossy().to_string();
        let out2 = search_files(&pattern2, None).unwrap();
        let json2 = success_json(out2);
        for m in json2["matches"].as_array().unwrap() {
            let s = m.as_str().unwrap().replace('\\', "/");
            let comps: Vec<&str> = s.split('/').collect();
            assert!(!comps.contains(&".git"), "matched {s} under .git");
            assert!(!comps.contains(&"target"), "matched {s} under target");
        }
        assert!(json2["skipped"].as_u64().unwrap() >= 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_files_max_results() {
        let dir = temp_dir("cap");
        for i in 0..600 {
            write(dir.join(format!("f{i:03}.txt")).to_str().unwrap(), "x");
        }
        let pattern = dir.join("*").to_string_lossy().to_string();
        let out = search_files(&pattern, None).unwrap();
        let json = success_json(out);
        assert_eq!(json["count"].as_u64().unwrap(), 500);
        assert!(json["truncated"].as_bool().unwrap());
        let out = search_files(&pattern, Some(42)).unwrap();
        let json = success_json(out);
        assert_eq!(json["count"].as_u64().unwrap(), 42);
        assert!(json["truncated"].as_bool().unwrap());
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

    #[test]
    fn test_apply_diff_crlf_multiline() {
        // Multi-line SEARCH block (LF) against a CRLF file must match and
        // the file must stay CRLF after the edit.
        let dir = temp_dir("diffcrlfm");
        let p = dir.join("crlfm.txt");
        fs::write(&p, "alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let diff = "<<<<<<< SEARCH\nalpha\nbeta\n=======\nALPHA\nBETA\n>>>>>>> REPLACE\n";
        let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
        let json = success_json(out);
        assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
        let content = read_string(p.to_str().unwrap());
        assert_eq!(content, "ALPHA\r\nBETA\r\ngamma\r\n", "content: {content:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_crlf_diff_payload() {
        // A diff payload with CRLF line endings must also work on a CRLF file.
        let dir = temp_dir("diffcrlfd");
        let p = dir.join("crlfd.txt");
        fs::write(&p, "one\r\ntwo\r\n").unwrap();
        let diff = "<<<<<<< SEARCH\r\none\r\ntwo\r\n=======\r\nONE\r\n>>>>>>> REPLACE\r\n";
        apply_diff(p.to_str().unwrap(), diff).unwrap();
        let content = read_string(p.to_str().unwrap());
        assert_eq!(content, "ONE\r\n", "content: {content:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_diff_lf_file_stays_lf() {
        // Editing an LF file must not introduce CR bytes.
        let dir = temp_dir("difflf");
        let p = dir.join("lf.txt");
        fs::write(&p, "a\nb\nc\n").unwrap();
        let diff = "<<<<<<< SEARCH\na\nb\n=======\nA\nB\n>>>>>>> REPLACE\n";
        apply_diff(p.to_str().unwrap(), diff).unwrap();
        let bytes = fs::read(p.to_str().unwrap()).unwrap();
        assert_eq!(bytes, b"A\nB\nc\n", "LF file must not gain CR bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_mkdir_recursive() {
        let dir = temp_dir("mkdir");
        let nested = dir.join("a/b/c");
        mkdir(nested.to_str().unwrap(), true).unwrap();
        assert!(nested.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_mkdir_nonrecursive_missing_parent_fails() {
        let dir = temp_dir("mkdirnr");
        let nested = dir.join("a/b");
        let err = mkdir(nested.to_str().unwrap(), false).unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_file() {
        let dir = temp_dir("del");
        let p = dir.join("d.txt");
        write(p.to_str().unwrap(), "x");
        delete(p.to_str().unwrap(), false).unwrap();
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_dir_nonempty_guard_and_recursive() {
        let dir = temp_dir("deldir");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let inner = sub.join("in.txt");
        write(inner.to_str().unwrap(), "x");
        // Non-recursive delete of a non-empty dir must fail and keep contents.
        assert!(delete(sub.to_str().unwrap(), false).is_err());
        assert!(inner.exists(), "file must survive a failed non-recursive delete");
        delete(sub.to_str().unwrap(), true).unwrap();
        assert!(!sub.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_copy_file_and_dir() {
        let dir = temp_dir("cp");
        let src = dir.join("src");
        fs::create_dir_all(&src).unwrap();
        let f = src.join("f.txt");
        write(f.to_str().unwrap(), "data");
        let nested = src.join("n");
        fs::create_dir_all(&nested).unwrap();
        write(nested.join("n.txt").to_str().unwrap(), "nested");
        let dest = dir.join("dest");
        copy(src.to_str().unwrap(), dest.to_str().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(dest.join("f.txt")).unwrap(), "data");
        assert_eq!(fs::read_to_string(dest.join("n/n.txt")).unwrap(), "nested");
        // Plain file copy.
        let dest2 = dir.join("dest2.txt");
        copy(f.to_str().unwrap(), dest2.to_str().unwrap()).unwrap();
        assert_eq!(read_string(dest2.to_str().unwrap()), "data");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_copy_missing_source_fails() {
        let dir = temp_dir("cpnf");
        let err = copy(
            dir.join("nope").to_str().unwrap(),
            dir.join("nope2").to_str().unwrap(),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_move_file_and_dir() {
        let dir = temp_dir("mv");
        let p = dir.join("m.txt");
        write(p.to_str().unwrap(), "x");
        let p2 = dir.join("m2.txt");
        move_item(p.to_str().unwrap(), p2.to_str().unwrap()).unwrap();
        assert!(!p.exists());
        assert_eq!(read_string(p2.to_str().unwrap()), "x");

        let d1 = dir.join("d1");
        fs::create_dir_all(&d1).unwrap();
        write(d1.join("x.txt").to_str().unwrap(), "y");
        let d2 = dir.join("d2");
        move_item(d1.to_str().unwrap(), d2.to_str().unwrap()).unwrap();
        assert!(!d1.exists());
        assert_eq!(fs::read_to_string(d2.join("x.txt")).unwrap(), "y");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_move_missing_source_fails() {
        let dir = temp_dir("mvnf");
        let err =
            move_item(dir.join("nope").to_str().unwrap(), dir.join("nope2").to_str().unwrap())
                .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_file_info_file_and_dir() {
        let dir = temp_dir("info");
        let p = dir.join("i.txt");
        write(p.to_str().unwrap(), "12345");
        let json = success_json(file_info(p.to_str().unwrap()).unwrap());
        assert_eq!(json["size"].as_u64().unwrap(), 5);
        assert!(json["is_file"].as_bool().unwrap());
        assert!(!json["is_dir"].as_bool().unwrap());
        assert!(json["permissions"].as_str().is_some());
        let dir_json = success_json(file_info(dir.to_str().unwrap()).unwrap());
        assert!(dir_json["is_dir"].as_bool().unwrap());
        assert!(!dir_json["is_file"].as_bool().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_file_info_missing_fails() {
        let dir = temp_dir("infonf");
        let err = file_info(dir.join("nope").to_str().unwrap()).unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
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
    fn test_mkdir_tool_executes() {
        let dir = temp_dir("toolmkdir");
        let nested = dir.join("x/y");
        let tool = MkdirTool::new();
        assert_eq!(tool.name(), "mkdir");
        let out = tool
            .execute(params(&[("path", nested.to_str().unwrap())]))
            .unwrap();
        let json = success_json(out);
        assert!(json["created"].as_bool().unwrap());
        assert!(nested.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_tool_executes() {
        let dir = temp_dir("tooldel");
        let p = dir.join("t.txt");
        write(p.to_str().unwrap(), "x");
        let tool = DeleteTool::new();
        assert_eq!(tool.name(), "delete");
        let out = tool
            .execute(params(&[("path", p.to_str().unwrap())]))
            .unwrap();
        let json = success_json(out);
        assert!(json["deleted"].as_bool().unwrap());
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_copy_move_fileinfo_tools_executes() {
        let dir = temp_dir("toolcmi");
        let p = dir.join("a.txt");
        write(p.to_str().unwrap(), "data");

        let cp = CopyTool::new();
        assert_eq!(cp.name(), "copy");
        let p2 = dir.join("b.txt");
        cp.execute(params(&[
            ("src", p.to_str().unwrap()),
            ("dest", p2.to_str().unwrap()),
        ]))
        .unwrap();
        assert_eq!(read_string(p2.to_str().unwrap()), "data");

        let mv = MoveTool::new();
        assert_eq!(mv.name(), "move");
        let p3 = dir.join("c.txt");
        mv.execute(params(&[
            ("src", p2.to_str().unwrap()),
            ("dest", p3.to_str().unwrap()),
        ]))
        .unwrap();
        assert!(!p2.exists());
        assert_eq!(read_string(p3.to_str().unwrap()), "data");

        let info = FileInfoTool::new();
        assert_eq!(info.name(), "file_info");
        let json = success_json(
            info.execute(params(&[("path", p.to_str().unwrap())]))
                .unwrap(),
        );
        assert_eq!(json["size"].as_u64().unwrap(), 4);
        assert!(json["is_file"].as_bool().unwrap());
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
        let mkdir = MkdirTool::new();
        let del = DeleteTool::new();
        let cp = CopyTool::new();
        let mv = MoveTool::new();
        let info = FileInfoTool::new();
        let names = vec![
            read.name(),
            write.name(),
            append.name(),
            list.name(),
            search.name(),
            diff.name(),
            mkdir.name(),
            del.name(),
            cp.name(),
            mv.name(),
            info.name(),
        ];
        let set: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(set.len(), 11, "tool names must be unique: {names:?}");
    }
}
