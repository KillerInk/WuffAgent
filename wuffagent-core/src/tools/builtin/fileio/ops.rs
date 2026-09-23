//! The file-operation functions backing the builtin file tools (read/
//! write/append/list/search/apply_diff/mkdir/delete/copy/move/info).
//! Pure code motion from the old tools/builtin/file_io.rs (F1).

use std::fs;

use crate::tools::types::{ToolError, ToolOutput};

use super::common::*;

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
///
/// A UTF-8 BOM is stripped from the first line (so the model sees clean
/// text and can copy it into `apply_diff` SEARCH blocks) and reported in
/// the `bom` output flag. CRLF files are returned as LF lines — `write_file`
/// and `apply_diff` restore the file's original line ending on save.
pub(crate) fn read_file(
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

    let file = fs::File::open(path)
        .map_err(|e| ToolError::Execution(format!("Failed to open '{}': {}", path, e)))?;
    let mut reader = std::io::BufReader::new(file);
    // Fail early with a clear message on UTF-16 files (BOM sniff) instead of
    // erroring mid-iteration with "stream did not contain valid UTF-8".
    if let Ok(buf) = reader.fill_buf() {
        if buf.starts_with(&[0xFF, 0xFE]) || buf.starts_with(&[0xFE, 0xFF]) {
            return Err(ToolError::Execution(format!(
                "File '{}' appears to be UTF-16 encoded (BOM detected); only UTF-8 files are supported",
                path
            )));
        }
    }

    // Params are 1-based; convert to 0-based internally.
    let start = start_line.map(|n| n - 1).unwrap_or(0);
    let end = end_line.map(|n| n - 1); // None means read until EOF
    let mut result = String::new();
    let mut line_idx = 0usize;
    let mut lines_returned = 0usize;
    let mut total_lines = 0usize;
    let mut truncated = false;

    let mut had_bom = false;
    for line in reader.lines() {
        let raw = line.map_err(|e| {
            ToolError::Execution(format!("Failed to read line {}: {}", line_idx + 1, e))
        })?;
        // A UTF-8 BOM only ever occurs at the very start of the file; strip
        // it so the model sees (and can re-use) clean text.
        let line: &str = if line_idx == 0 {
            had_bom = raw.starts_with(UTF8_BOM);
            strip_utf8_bom(&raw)
        } else {
            &raw
        };
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
            line.to_string()
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
        "bom": had_bom,
    })))
}

/// List directory entries with per-entry metadata: `type` (dir/file/
/// symlink/other) and `size` in bytes for regular files. Directories are
/// listed first, then files, each sorted by name — the OS enumeration
/// order is otherwise arbitrary.
pub(crate) fn list_dir(path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let dir_iter = fs::read_dir(path)
        .map_err(|e| ToolError::Execution(format!("Failed to list '{}': {}", path, e)))?;
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

/// Glob search with `*`, `?`, `[cls]`, and `**` (recursive) support.
///
/// Matches under `.git` or `target` directories are skipped (VCS metadata
/// and build artifacts are noise), and returned matches are capped
/// (`max_results`, default 500) with a `truncated` flag when the cap stops
/// the walk early.
pub(crate) fn search_files(
    pattern: &str,
    max_results: Option<usize>,
) -> crate::tools::types::ToolResult<ToolOutput> {
    /// Default cap on matches returned (keeps output usable and encourages
    /// narrower patterns for huge searches).
    const DEFAULT_MAX_RESULTS: usize = 500;
    /// Directory components always skipped.
    const SKIP_DIRS: &[&str] = &[".git", "target"];

    let cap = max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);
    let mut iter = glob::glob(pattern)
        .map_err(|e| ToolError::Execution(format!("Invalid glob pattern '{}': {}", pattern, e)))?;

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

/// Create a directory. If `recursive` is true, parent directories are
/// created as needed (`mkdir -p` semantics).
pub(crate) fn mkdir(path: &str, recursive: bool) -> crate::tools::types::ToolResult<ToolOutput> {
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
pub(crate) fn delete(path: &str, recursive: bool) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(path)
        .map_err(|e| ToolError::Execution(format!("Failed to stat '{}': {}", path, e)))?;
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
pub(crate) fn copy(src: &str, dest: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(src)
        .map_err(|e| ToolError::Execution(format!("Source '{}' not found: {}", src, e)))?;
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
pub(crate) fn copy_dir_all(src: &str, dest: &str) -> std::io::Result<()> {
    copy_dir_all_inner(src, dest, &mut std::collections::HashSet::new())
}

pub(crate) fn copy_dir_all_inner(
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
pub(crate) fn move_item(src: &str, dest: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    if fs::metadata(src).is_err() {
        return Err(ToolError::Execution(format!("Source '{}' not found", src)));
    }
    if fs::rename(src, dest).is_err() {
        // Try cross-device fallback
        copy(src, dest)?;
        fs::remove_file(src)
            .or_else(|_| fs::remove_dir_all(src))
            .map_err(|e| {
                ToolError::Execution(format!("Cross-device move failed after copy: {}", e))
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
pub(crate) fn file_info(path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let metadata = fs::metadata(path)
        .map_err(|e| ToolError::Execution(format!("Failed to stat '{}': {}", path, e)))?;
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

