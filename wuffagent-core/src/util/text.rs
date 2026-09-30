//! Shared text helpers: char/byte-aware truncation.
//!
//! Consolidates the per-module truncation variants that all do the same
//! thing (cap a string, mark the cut with `…`):
//! - [`truncate_chars`] — the common case: at most `max` CHARACTERS + `…`
//!   (was duplicated as `agents::agent::truncate_chars`,
//!   `improvement::truncate_to`, `improvement::truncate_for_evidence`).
//! - [`truncate_bytes`] — at most `max_bytes` BYTES, backing off to the
//!   previous char boundary + `…` (was `tools::builtin::search::truncate_line`).
//!
//! Deliberately NOT merged in (different semantics, kept at their call
//! sites): `truncate_note` (reserves the ellipsis slot: `max - 1` + `…`),
//! `mcp::tool::truncate_to_bytes` (byte cap, no ellipsis suffix),
//! `builtin::shell::truncate_output` (byte cap + its own marker text).

/// Truncate to at most `max` characters, appending `…` if a cut happened.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Truncate to at most `max_bytes` bytes (char-boundary safe), appending
/// `…` if a cut happened.
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_short_string_unchanged() {
        assert_eq!(truncate_chars("hi", 5), "hi");
        assert_eq!(truncate_chars("", 3), "");
    }

    #[test]
    fn truncate_chars_caps_and_marks() {
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        // Exactly at the cap: no cut.
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        // "é" is 2 bytes; the cut must land on a char boundary.
        let s = "ééééé";
        let t = truncate_chars(s, 3);
        assert_eq!(t, "ééé…");
    }

    #[test]
    fn truncate_bytes_short_string_unchanged() {
        assert_eq!(truncate_bytes("hi", 5), "hi");
        assert_eq!(truncate_bytes("", 3), "");
    }

    #[test]
    fn truncate_bytes_caps_and_marks() {
        assert_eq!(truncate_bytes("hello world", 5), "hello…");
        assert_eq!(truncate_bytes("hello", 5), "hello");
    }

    #[test]
    fn truncate_bytes_backs_off_to_char_boundary() {
        // "hello é": "é" occupies bytes 6..8. Cutting at 7 lands inside it and
        // must back off to boundary 6 (the space); cutting at 8 is exactly on
        // the next boundary and keeps the full "é".
        assert_eq!(truncate_bytes("hello é", 7), "hello …");
        assert_eq!(truncate_bytes("hello é", 8), "hello é"); // len 8 <= max 8: unchanged
        // A cut exactly on a boundary (6) is not a back-off: the space stays.
        assert_eq!(truncate_bytes("hello é", 6), "hello …");
    }
}
