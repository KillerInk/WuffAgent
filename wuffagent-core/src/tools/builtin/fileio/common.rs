//! File I/O plumbing: path validation/sensitivity, UTF-8 BOM helpers,
//! line-ending (Eol) detection/normalization, low-level read/replace
//! helpers. Pure code motion from the old tools/builtin/file_io.rs (F1).

use std::fs;
use std::path::Path;

use crate::tools::types::ToolError;

/// Validates a path for safety, rejecting traversal patterns and sensitive
/// system directories. Canonicalizes when possible for stronger guarantees.
pub(crate) fn validate_path(path: &str) -> Result<(), ToolError> {
    if path.is_empty() || path.len() > 4096 {
        return Err(ToolError::Execution("Path not allowed".to_string()));
    }

    let normalized = path.replace('\\', "/");
    if normalized.contains("../") || normalized.ends_with("/..") || normalized == ".." {
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
        let canonical_str = canonical_str
            .strip_prefix(r"\\?\")
            .unwrap_or(&canonical_str);
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

pub(crate) fn is_sensitive_path(p: &str) -> bool {
    p == "/etc"
        || p.starts_with("/etc/")
        || p == "/root"
        || p.starts_with("/root/")
        || p == "c:/windows"
        || p.starts_with("c:/windows/")
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
pub(crate) enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
        }
    }
}

/// Detect the dominant line ending by counting CRLF vs standalone LF, so a
/// single stray CRLF in an otherwise-LF file does not flip the result.
/// Text without any line break reports Lf (any conversion is a no-op then).
pub(crate) fn detect_eol(bytes: &[u8]) -> Eol {
    let crlf = bytes.windows(2).filter(|w| *w == b"\r\n").count();
    let lone_lf = bytes
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        .saturating_sub(crlf);
    if crlf > lone_lf {
        Eol::Crlf
    } else {
        Eol::Lf
    }
}

/// Rewrite a payload's line endings to `target` when the payload uses a
/// single uniform ending different from the target. Mixed payloads and
/// payloads without line breaks are returned unchanged.
pub(crate) fn normalize_eol(payload: &str, target: Eol) -> std::borrow::Cow<'_, str> {
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
pub(crate) fn sniff_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 8 * 1024];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

/// Last byte of a file, if the file exists and is non-empty.
pub(crate) fn last_byte(path: &Path) -> Option<u8> {
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
pub(crate) fn read_text_file(path: &str) -> Result<String, ToolError> {
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


/// Rename `tmp` over `target`, replacing an existing file.
///
/// On Unix `fs::rename` replaces atomically. On Windows `fs::rename`
/// refuses to overwrite an existing target, so we remove it first and
/// retry — the target may briefly not exist, but it is never
/// half-written, which is the invariant that matters here.
pub(crate) fn replace_over_existing(tmp: &Path, target: &Path) -> std::io::Result<()> {
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

