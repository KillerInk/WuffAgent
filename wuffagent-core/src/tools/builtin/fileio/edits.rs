//! The file-modifying operations: `write_file`, `append_file`, and the
//! SEARCH/REPLACE `apply_diff` machinery. Split out of `fileio/ops.rs`
//! (Phase D size split).

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::tools::types::{ToolError, ToolOutput};

use super::common::*;

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
pub(crate) fn write_file(path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
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
        return Err(ToolError::Execution(format!(
            "Failed to write '{}': {}",
            path, e
        )));
    }
    if let Err(e) = replace_over_existing(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(ToolError::Execution(format!(
            "Failed to replace '{}': {}",
            path, e
        )));
    }
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "bytes_written": content.len(),
        "success": true,
    })))
}

/// Append content to the end of a file.
///
/// The existing file's dominant line ending is applied to the appended
/// payload (so a CRLF file does not gain LF lines), and when the existing
/// file does not end with a line break one is inserted first (in the file's
/// line ending) so the appended content starts on its own line instead of
/// concatenating mid-line.
pub(crate) fn append_file(path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
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
        .map_err(|e| ToolError::Execution(format!("Failed to append to '{}': {}", path, e)))?;
    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "bytes_appended": payload.len(),
        "success": true,
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
pub(crate) fn apply_diff(path: &str, diff: &str) -> crate::tools::types::ToolResult<ToolOutput> {
    let raw = read_text_file(path)?;
    let had_bom = raw.starts_with(UTF8_BOM);
    let content = strip_utf8_bom(&raw);
    let diff = strip_utf8_bom(diff);

    let blocks = parse_search_replace_blocks(diff).map_err(|e| {
        ToolError::Execution(format!(
            "Malformed search/replace blocks for '{}': {}",
            path, e
        ))
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
                "Block {}: search text not found in '{}'",
                i + 1,
                path
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
pub(crate) fn parse_search_replace_blocks(diff: &str) -> Result<Vec<(String, String)>, String> {
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
