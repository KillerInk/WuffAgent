//! Display-timestamp formatting and tolerant parsing helpers.

/// Format the current time as a human-readable timestamp string.
///
/// Includes the local date (`YYYY-MM-DD HH:MM:SS`) so the UI can draw day
/// separators. Display-only field — never parsed by the model layer. Legacy
/// sessions stored the older 8-char `HH:MM:SS` form; see `timestamp_day` /
/// `timestamp_time` for tolerant parsing.
pub fn format_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The `YYYY-MM-DD` day part of a display timestamp, if it has one.
/// Returns `None` for legacy time-only timestamps (no separator is drawn).
pub fn timestamp_day(ts: &str) -> Option<&str> {
    if ts.len() >= 11 && ts.as_bytes()[4] == b'-' {
        Some(&ts[..10])
    } else {
        None
    }
}

/// The time part of a display timestamp for rendering, or the whole string
/// for legacy time-only timestamps.
pub fn timestamp_time(ts: &str) -> &str {
    if ts.len() >= 19 && ts.as_bytes()[10] == b' ' {
        &ts[11..]
    } else {
        ts
    }
}
