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
/// Tolerant fallback: when the exact search text matches nothing, the block
/// is retried as a whole-line match that ignores trailing whitespace (the
/// most common cause of spurious failures). A unique tolerant match is
/// applied and reported in the result as `fuzzy_blocks`.
///
/// Failure messages are self-correcting: an ambiguous match reports the line
/// numbers of every occurrence, and a not-found match reports the closest
/// matching file region(s) with line numbers, so the model can fix the SEARCH
/// text without a full file re-read.
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
    let mut fuzzy_blocks: u32 = 0;
    for (i, (search, replace)) in blocks.iter().enumerate() {
        let count = current.matches(search).count();
        if count == 1 {
            current = current.replacen(search, replace, 1);
            applied += 1;
            continue;
        }
        if count > 1 {
            return Err(ToolError::Execution(format!(
                "Block {}: search text matches {} places in '{}' (lines {}), add more context to make it unique",
                i + 1,
                count,
                path,
                match_line_numbers(&current, search)
            )));
        }

        // Exact match failed (0 matches): fall back to a whole-line match
        // that ignores trailing whitespace on every line.
        let lines: Vec<&str> = current.lines().collect();
        let mut search_lines: Vec<&str> = search.lines().collect();
        if search.ends_with('\n') {
            search_lines.push(""); // trailing blank payload line, lost by lines()
        }
        let hits = tolerant_match_positions(&lines, &search_lines);
        match hits.len() {
            1 => {
                let start = hits[0];
                let had_trailing = current.ends_with('\n');
                let mut new_lines: Vec<&str> = replace.lines().collect();
                if replace.ends_with('\n') {
                    new_lines.push("");
                }
                let mut out: Vec<&str> =
                    Vec::with_capacity(lines.len().saturating_sub(search_lines.len()) + new_lines.len());
                out.extend_from_slice(&lines[..start]);
                out.extend_from_slice(&new_lines);
                out.extend_from_slice(&lines[start + search_lines.len()..]);
                let mut result = out.join("\n");
                if had_trailing && !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
                current = result;
                applied += 1;
                fuzzy_blocks += 1;
            }
            0 => {
                return Err(ToolError::Execution(format!(
                    "Block {}: search text not found in '{}' (file has {} lines). The SEARCH text must match the file exactly — closest matching region(s) below; copy the actual lines from there (or read_file the area):\n{}",
                    i + 1,
                    path,
                    lines.len(),
                    closest_regions(&lines, &search_lines)
                )));
            }
            _ => {
                let line_nos = hits
                    .iter()
                    .take(10)
                    .map(|h| (h + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ToolError::Execution(format!(
                    "Block {}: search text matches multiple places in '{}' ignoring trailing whitespace (lines {}), add more context to make it unique",
                    i + 1, path, line_nos
                )));
            }
        }
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
        "fuzzy_blocks": fuzzy_blocks,
        "success": true,
    })))
}

/// 1-based line numbers of the (capped) occurrences of `needle` in
/// `haystack`, comma-joined. Diagnostics for the ambiguous-match error.
fn match_line_numbers(haystack: &str, needle: &str) -> String {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = haystack[from..].find(needle) {
        let abs = from + off;
        out.push((haystack[..abs].lines().count() + 1).to_string());
        if out.len() >= 10 {
            break;
        }
        from = abs + needle.len().max(1);
    }
    out.join(", ")
}

/// Whole-line match positions of `search_lines` in `lines` where every line
/// pair is equal ignoring trailing whitespace. Capped at 10 hits (the caller
/// treats >1 as ambiguous). Empty vec = no match.
fn tolerant_match_positions<'a>(lines: &[&'a str], search_lines: &[&str]) -> Vec<usize> {
    let n = search_lines.len();
    if n == 0 || n > lines.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for start in 0..=lines.len() - n {
        if (0..n).all(|k| lines[start + k].trim_end() == search_lines[k].trim_end()) {
            hits.push(start);
            if hits.len() >= 10 {
                break;
            }
        }
    }
    hits
}

/// Model-readable hint of where the search text probably belongs: up to 3
/// file regions whose lines best resemble the search block's first non-empty
/// line, each with 1-based line numbers. Used only in the not-found error so
/// the model can fix the SEARCH text without re-reading the file.
fn closest_regions(file_lines: &[&str], search_lines: &[&str]) -> String {
    let head: Vec<String> = file_lines
        .iter()
        .take(8)
        .enumerate()
        .map(|(i, l)| format!("{}: {}", i + 1, l))
        .collect();
    let head = if head.is_empty() {
        "(file is empty)".to_string()
    } else {
        head.join("\n")
    };

    let anchor: Option<&str> = search_lines.iter().map(|l| l.trim()).find(|l| !l.is_empty());
    let Some(anchor) = anchor else {
        // The whole search block is blank lines.
        return format!("(your SEARCH block is only blank lines; file head):\n{head}");
    };
    let anchor = truncate_char(anchor, 200);

    let mut scored: Vec<(usize, usize)> = file_lines
        .iter()
        .take(4000)
        .enumerate()
        .map(|(i, l)| (common_substring_len(truncate_char(l, 200), anchor), i))
        .filter(|(score, _)| *score >= 4)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let n = search_lines.len().max(1);
    let mut picks: Vec<usize> = Vec::new();
    for (_, i) in scored {
        if picks.iter().all(|&p| p.max(i) - p.min(i) >= n) {
            picks.push(i);
            if picks.len() == 3 {
                break;
            }
        }
    }
    picks.sort_unstable();
    if picks.is_empty() {
        return format!(
            "(no similar lines found — the SEARCH text may be from an older version of the file; read_file to refresh)\nfile head:\n{head}"
        );
    }
    let mut parts = Vec::new();
    for start in picks {
        let end = (start + n).min(file_lines.len());
        let region: Vec<String> = file_lines[start..end]
            .iter()
            .enumerate()
            .map(|(k, l)| format!("{}: {}", start + k + 1, l))
            .collect();
        let mut region = region.join("\n");
        if region.len() > 600 {
            region.truncate(600);
            region.push_str(" …");
        }
        parts.push(region);
    }
    parts.join("\n…\n")
}

/// `&s[..s.len().min(max)]` but char-boundary safe (never panics on
/// multi-byte boundaries).
fn truncate_char(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Length of the longest common substring of `a` and `b` (naive DP; the
/// caller caps input lengths so this stays cheap on the error path).
fn common_substring_len(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut best = 0usize;
    for &ca in &a {
        let mut curr = vec![0usize; b.len() + 1];
        for j in 1..=b.len() {
            if ca == b[j - 1] {
                curr[j] = prev[j - 1] + 1;
                if curr[j] > best {
                    best = curr[j];
                }
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    best
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

/// Replace a line range in an existing file.
///
/// The model names WHICH lines to replace (1-indexed, inclusive) and the
/// replacement text — no exact-text search needed.
///
/// `verify_contains` is an optional guard: when given, the targeted lines
/// must contain that text (substring match, line-ending normalized) or the
/// tool fails and reports the actual targeted lines. This catches off-by-N
/// line numbers when the model edited a file it read earlier.
///
/// Line endings and BOM are handled exactly like `apply_diff`: matching
/// happens on a line-split view of the file, and the file's dominant line
/// ending (CRLF vs LF decided by majority) and UTF-8 BOM are restored before
/// writing, so editing a CRLF file never rewrites it as LF. The original
/// trailing-newline state of the file is preserved.
///
/// Failure never touches the file; error messages are self-correcting
/// (they report the total line count or the actual targeted lines so the
/// model can retry without a full re-read).
pub(crate) fn replace_lines(
    path: &str,
    start_line: usize,
    end_line: usize,
    new_content: &str,
    verify_contains: Option<&str>,
) -> crate::tools::types::ToolResult<ToolOutput> {
    let raw = read_text_file(path)?;
    let had_bom = raw.starts_with(UTF8_BOM);
    let content = strip_utf8_bom(&raw);
    let eol = detect_eol(content.as_bytes());
    let had_trailing_newline = content.ends_with('\n') || content.ends_with('\r');

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Err(ToolError::Execution(format!(
            "File '{}' is empty (no lines to replace); use write_file instead",
            path
        )));
    }
    if start_line < 1 {
        return Err(ToolError::Execution(format!(
            "start_line must be >= 1 (1-indexed); got {} in '{}'",
            start_line, path
        )));
    }
    if start_line > end_line {
        return Err(ToolError::Execution(format!(
            "start_line ({}) is after end_line ({}) in '{}'; the range must target at least one existing line",
            start_line, end_line, path
        )));
    }
    if end_line > total {
        let from = total.saturating_sub(7).max(1);
        let snippet: String = lines[from - 1..]
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}: {}", from + i, l))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ToolError::Execution(format!(
            "end_line ({}) is beyond the end of '{}' which has {} lines; last lines:\n{}",
            end_line,
            path,
            total,
            snippet
        )));
    }

    let (lo, hi) = (start_line - 1, end_line);
    if let Some(verify) = verify_contains {
        let verify = verify.replace("\r\n", "\n");
        let targeted = lines[lo..hi].join("\n");
        if !targeted.contains(verify.as_str()) {
            return Err(ToolError::Execution(format!(
                "verify_contains text not found in lines {}–{} of '{}'; targeted lines:\n{}",
                start_line,
                end_line,
                path,
                targeted
            )));
        }
    }

    let replacement: Vec<&str> = new_content.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(total - (hi - lo) + replacement.len());
    out.extend_from_slice(&lines[..lo]);
    out.extend_from_slice(&replacement);
    out.extend_from_slice(&lines[hi..]);

    let sep = eol.as_str();
    let mut result = out.join(sep);
    if had_trailing_newline && !result.is_empty() && !result.ends_with(sep) {
        result.push_str(sep);
    }
    if had_bom {
        result.insert(0, UTF8_BOM);
    }
    fs::write(path, &result).map_err(|e| {
        ToolError::Execution(format!("Failed to write patched file '{}': {}", path, e))
    })?;

    Ok(ToolOutput::Success(serde_json::json!({
        "path": path,
        "start_line": start_line,
        "end_line": end_line,
        "lines_replaced": hi - lo,
        "lines_inserted": replacement.len(),
        "verified": verify_contains.is_some(),
        "success": true,
    })))
}
