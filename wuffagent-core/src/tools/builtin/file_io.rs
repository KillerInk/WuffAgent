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

// ─── Encoding / line-ending helpers ─────────────────────────────────────────

/// UTF-8 byte-order mark, in decoded text form.
pub(crate) const UTF8_BOM: char = '\u{feff}';
/// UTF-8 BOM as raw bytes.
pub(crate) const UTF8_BOM_BYTES: &[u8] = b"\xEF\xBB\xBF";

/// Strip a leading UTF-8 BOM from decoded text (no-op if absent).
pub(crate) fn strip_utf8_bom(s: &str) -> &str {
    s.strip_prefix(UTF8_BOM).unwrap_or(s)
}

/// Dominant line ending of a file or text payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
        }
    }
}

/// Detect the dominant line ending by counting CRLF vs standalone LF, so a
/// single stray CRLF in an otherwise-LF file does not flip the result.
/// Text without any line break reports Lf (any conversion is a no-op then).
fn detect_eol(bytes: &[u8]) -> Eol {
    let crlf = bytes.windows(2).filter(|w| *w == b"\r\n").count();
    let lone_lf = bytes.iter().filter(|&&b| b == b'\n').count().saturating_sub(crlf);
    if crlf > lone_lf {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

/// Rewrite a payload's line endings to `target` when the payload uses a
/// single uniform ending different from the target. Mixed payloads and
/// payloads without line breaks are returned unchanged.
fn normalize_eol(payload: &str, target: Eol) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match target {
        Eol::Lf if payload.contains("\r\n") => Cow::Owned(payload.replace("\r\n", "\n")),
        Eol::Crlf if !payload.contains("\r\n") && payload.contains('\n') => {
            Cow::Owned(payload.replace('\n', "\r\n"))
        }
        _ => Cow::Borrowed(payload),
    }
}

/// Read up to the first 8 KB of an existing file, for BOM / line-ending
/// sniffing. Returns None if the file cannot be opened.
fn sniff_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 8 * 1024];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

/// Last byte of a file, if the file exists and is non-empty.
fn last_byte(path: &Path) -> Option<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() == 0 {
        return None;
    }
    file.seek(SeekFrom::End(-1)).ok()?;
    let mut b = [0u8; 1];
    file.read_exact(&mut b).ok()?;
    Some(b[0])
}

/// Read a UTF-8 text file with clear errors: a UTF-16 BOM yields a specific
/// message (a non-UTF-8 BOM almost always means UTF-16), and invalid UTF-8
/// no longer surfaces as the cryptic "stream did not contain valid UTF-8".
fn read_text_file(path: &str) -> Result<String, ToolError> {
    let bytes = fs::read(path)
        .map_err(|e| ToolError::Execution(format!("Failed to read '{}': {}", path, e)))?;
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(ToolError::Execution(format!(
            "File '{}' appears to be UTF-16 encoded (BOM detected); only UTF-8 files are supported",
            path
        )));
    }
    String::from_utf8(bytes).map_err(|_| {
        ToolError::Execution(format!(
            "File '{}' is not valid UTF-8; only UTF-8 text files are supported",
            path
        ))
    })
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
///
/// A UTF-8 BOM is stripped from the first line (so the model sees clean
/// text and can copy it into `apply_diff` SEARCH blocks) and reported in
/// the `bom` output flag. CRLF files are returned as LF lines — `write_file`
/// and `apply_diff` restore the file's original line ending on save.
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
///
/// When overwriting an existing file, its BOM and dominant line ending
/// are preserved: the model only ever emits LF (that is what `read_file`
/// shows), so without this a read → edit → write cycle would silently
/// convert CRLF files to LF and drop UTF-8 BOMs.
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

    // Preserve the existing target's BOM and dominant line ending (see
    // function docs). New files are written verbatim.
    let content = match sniff_head(target) {
        Some(head) => {
            let mut out = normalize_eol(content, detect_eol(&head)).into_owned();
            if head.starts_with(UTF8_BOM_BYTES) && !out.starts_with(UTF8_BOM) {
                out.insert(0, UTF8_BOM);
            }
            out
        }
        None => content.to_string(),
    };

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
///
/// The existing file's dominant line ending is applied to the appended
/// payload (so a CRLF file does not gain LF lines), and when the existing
/// file does not end with a line break one is inserted first (in the file's
/// line ending) so the appended content starts on its own line instead of
/// concatenating mid-line.
fn append_file(path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let target = Path::new(path);
    let payload = match sniff_head(target) {
        Some(head) => {
            let eol = detect_eol(&head);
            let mut out = normalize_eol(content, eol).into_owned();
            if !out.is_empty() && matches!(last_byte(target), Some(b) if b != b'\n') {
                out.insert_str(0, eol.as_str());
            }
            out
        }
        None => content.to_string(),
    };
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            ToolError::Execution(format!("Failed to open '{}' for appending: {}", path, e))
        })?
        .write_all(payload.as_bytes())
        .map_err(|e| {
            ToolError::Execution(format!("Failed to append to '{}': {}", path, e))
        })?;
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "bytes_appended": payload.len(),
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
/// dominant line ending (CRLF vs LF decided by majority, so one stray CRLF
/// in an LF file does not flip the result) is restored before writing, so
/// editing a CRLF file never rewrites it as LF.
///
/// BOM: a UTF-8 BOM (in the file or at the start of the diff payload) is
/// bookkeeping, not content — it is stripped before matching and restored
/// on write, so SEARCH text for the first line does not need to (and
/// cannot) contain the invisible BOM character.
fn apply_diff(path: &str, diff: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let raw = read_text_file(path)?;
    let had_bom = raw.starts_with(UTF8_BOM);
    let content = strip_utf8_bom(&raw);
    let diff = strip_utf8_bom(diff);

    let blocks = parse_search_replace_blocks(diff).map_err(|e| {
        ToolError::Execution(format!("Malformed search/replace blocks for '{}': {}", path, e))
    })?;
    if blocks.is_empty() {
        return Err(ToolError::Execution(
            "No search/replace blocks found in diff".to_string(),
        ));
    }

    // Match on a line-ending-normalized copy (see function docs).
    let eol = detect_eol(content.as_bytes());
    let mut current: String = if eol == Eol::Crlf {
        content.replace("\r\n", "\n")
    } else {
        content.to_string()
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

    if eol == Eol::Crlf {
        current = current.replace('\n', "\r\n");
    }
    if had_bom {
        current.insert(0, UTF8_BOM);
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
        "Read a text file, optionally limited to a line range. Returns the file content; large files are truncated with a `truncated` flag and `total_lines` for paging. A UTF-8 BOM is stripped from the first line (reported in the `bom` flag); CRLF files are shown as LF lines."
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
        "Write (overwrite) a text file. The full content must be provided. Parent directories are created if missing; the write is atomic (temp file + rename), so a failure never leaves a truncated file. When overwriting an existing file, its BOM and dominant line ending (CRLF/LF) are preserved, so LF content rewrites a CRLF file as CRLF."
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
        "Append content to the end of a file (creates the file if it does not exist). Line endings are matched to the existing file, and a missing trailing line break in the existing file is added before the appended content."
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
         Use read_file first to copy the exact current text. \
         The file's line ending (CRLF/LF) and UTF-8 BOM are preserved."
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
mod tests;
