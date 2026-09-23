//! Pure aggregation over the usage JSONL log: tolerant loading, hour/day/week
//! bucketing with zero-filled windows, and an incremental reader for the UI.
//!
//! All bucket math is done on local **wall-clock** `NaiveDateTime`s
//! (converted from the UTC `ts` once per entry). Wall-clock arithmetic is
//! exact calendar math — no duration division across timezones — so DST
//! transitions cannot shift bucket boundaries; a repeated "fold" hour during
//! fall-back simply lands in the same wall-clock bucket.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, Timelike, Utc};

/// Bucket granularity (also selects the window size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// One bucket per hour; window = the last 24 hours.
    Hour,
    /// One bucket per day; window = the last 30 days.
    Day,
    /// One bucket per week (Monday-based); window = the last 12 weeks.
    Week,
}

impl Granularity {
    /// Number of buckets in the window for this granularity.
    pub fn bucket_count(self) -> usize {
        match self {
            Granularity::Hour => 24,
            Granularity::Day => 30,
            Granularity::Week => 12,
        }
    }

    /// The local wall-clock start of the first bucket in the window ending at
    /// `now_local`.
    fn window_start(&self, now_local: NaiveDateTime) -> NaiveDateTime {
        let days = chrono::Duration::days;
        match self {
            Granularity::Hour => {
                let hour_start = now_local
                    .date()
                    .and_hms_opt(now_local.hour(), 0, 0)
                    .expect("valid hour");
                hour_start - chrono::Duration::hours(23)
            }
            Granularity::Day => (now_local.date() - days(29)).and_hms_opt(0, 0, 0).unwrap(),
            Granularity::Week => monday_of(now_local.date()) - days(7 * 11),
        }
    }

    /// Zero-based index of the bucket containing `nd` (local wall clock), or
    /// `None` when the timestamp falls outside the window
    /// (`window_start..window_start + bucket_count * unit`).
    fn bucket_index(&self, nd: NaiveDateTime, window_start: NaiveDateTime) -> Option<usize> {
        let n = self.bucket_count();
        match self {
            Granularity::Hour => {
                let i = (nd - window_start).num_hours();
                (0..n as i64).contains(&i).then_some(i as usize)
            }
            Granularity::Day => {
                let i = (nd.date() - window_start.date()).num_days();
                (0..n as i64).contains(&i).then_some(i as usize)
            }
            Granularity::Week => {
                let i = (monday_of(nd.date()) - window_start).num_days() / 7;
                (0..n as i64).contains(&i).then_some(i as usize)
            }
        }
    }

    /// Local wall-clock start of bucket `i` (0-based) in the window.
    fn bucket_start(&self, window_start: NaiveDateTime, i: usize) -> NaiveDateTime {
        match self {
            Granularity::Hour => window_start + chrono::Duration::hours(i as i64),
            Granularity::Day => window_start + chrono::Duration::days(i as i64),
            Granularity::Week => window_start + chrono::Duration::days(7 * i as i64),
        }
    }
}

/// Monday (local wall clock, midnight) of the week containing `d`.
fn monday_of(d: NaiveDate) -> NaiveDateTime {
    (d - chrono::Duration::days(d.weekday().number_from_monday() as i64 - 1))
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

/// One bucket of aggregated usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    /// Local wall-clock start of the bucket (for axis labels / tooltips).
    pub start: NaiveDateTime,
    /// Sum of input (prompt) tokens.
    pub prompt_tokens: u64,
    /// Sum of output (completion) tokens.
    pub completion_tokens: u64,
    /// Sum of total tokens.
    pub total_tokens: u64,
    /// Number of LLM calls.
    pub calls: u32,
    /// Number of tool calls issued across the bucket's calls.
    pub tool_calls: u32,
    /// Character count of thinking/reasoning text across the bucket.
    pub thinking_chars: u64,
}

/// A zero-filled usage window: `buckets.len() == granularity.bucket_count()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    pub granularity: Granularity,
    /// Zero-filled buckets, oldest first.
    pub buckets: Vec<Bucket>,
    /// Window totals (equal to the sum over buckets).
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub calls: u32,
    /// Tool calls issued across the window.
    pub total_tool_calls: u32,
    /// Thinking/reasoning characters across the window.
    pub total_thinking_chars: u64,
}

/// Parse one JSONL line into an entry; `None` for empty or malformed lines.
pub fn parse_line(line: &str) -> Option<crate::usage::recorder::UsageEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// Load all entries from a JSONL file, tolerating malformed lines.
/// Returns `(entries, skipped_line_count)`. A missing file yields `([], 0)`.
pub fn load_entries(path: &Path) -> (Vec<crate::usage::recorder::UsageEntry>, usize) {
    let Ok(content) = fs::read_to_string(path) else {
        return (Vec::new(), 0);
    };
    let mut entries = Vec::new();
    let mut skipped = 0;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue; // blank lines are not "bad" lines
        }
        match parse_line(line) {
            Some(e) => entries.push(e),
            None => skipped += 1,
        }
    }
    (entries, skipped)
}

/// Aggregate entries into a zero-filled window ending at `now`.
///
/// `now` is in UTC; entries are converted to local wall clock before
/// bucketing. Entries outside the window (too old, or in the future) are
/// ignored.
pub fn bucketize(
    entries: &[crate::usage::recorder::UsageEntry],
    granularity: Granularity,
    now: DateTime<Utc>,
) -> UsageWindow {
    let now_local = now.with_timezone(&Local).naive_local();
    let window_start = granularity.window_start(now_local);
    let n = granularity.bucket_count();

    let mut buckets: Vec<Bucket> = (0..n)
        .map(|i| Bucket {
            start: granularity.bucket_start(window_start, i),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            calls: 0,
            tool_calls: 0,
            thinking_chars: 0,
        })
        .collect();

    let mut totals = [0u64; 3];
    let mut calls: u32 = 0;
    let mut tool_calls: u32 = 0;
    let mut thinking_chars: u64 = 0;
    for e in entries {
        let nd = e.ts.with_timezone(&Local).naive_local();
        let Some(i) = granularity.bucket_index(nd, window_start) else {
            continue;
        };
        let b = &mut buckets[i];
        b.prompt_tokens += e.prompt_tokens as u64;
        b.completion_tokens += e.completion_tokens as u64;
        b.total_tokens += e.total_tokens as u64;
        b.calls += 1;
        b.tool_calls += e.tool_calls;
        b.thinking_chars += e.thinking_chars;
        totals[0] += e.prompt_tokens as u64;
        totals[1] += e.completion_tokens as u64;
        totals[2] += e.total_tokens as u64;
        calls += 1;
        tool_calls += e.tool_calls;
        thinking_chars += e.thinking_chars;
    }

    UsageWindow {
        granularity,
        buckets,
        total_prompt_tokens: totals[0],
        total_completion_tokens: totals[1],
        total_tokens: totals[2],
        calls,
        total_tool_calls: tool_calls,
        total_thinking_chars: thinking_chars,
    }
}

/// Incremental reader for an append-only JSONL log.
///
/// Tracks the byte offset read so the UI can pick up only new lines each
/// frame (after a chat round completes). If the file shrank (truncated or
/// rewritten), the next `poll` transparently falls back to a full rescan.
/// An incomplete trailing line (torn write in progress) is buffered until
/// the newline arrives.
#[derive(Default, Debug)]
pub struct UsageLogReader {
    offset: u64,
    /// Incomplete trailing line from the previous read, if any.
    pending: String,
    /// Malformed lines skipped since the last full load.
    skipped: usize,
}

impl UsageLogReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Malformed lines skipped since the last full load.
    pub fn total_skipped(&self) -> usize {
        self.skipped
    }

    /// Forget all state and rescan the file from the beginning.
    pub fn reset(&mut self) {
        self.offset = 0;
        self.pending.clear();
        self.skipped = 0;
    }

    /// Read the entire file, replacing any previously read state.
    /// Returns `(entries, skipped_line_count)`.
    pub fn load_all(&mut self, path: &Path) -> (Vec<crate::usage::recorder::UsageEntry>, usize) {
        self.reset();
        let (entries, skipped) = load_entries(path);
        self.skipped = skipped;
        if let Ok(meta) = fs::metadata(path) {
            self.offset = meta.len();
        }
        (entries, skipped)
    }

    /// Read only the bytes appended since the last call.
    ///
    /// - File missing or unchanged: `([], 0)`.
    /// - File shrank below the tracked offset: full rescan.
    /// - A partial trailing line (no newline yet) is buffered, not skipped.
    ///
    /// Returns `(new_entries, newly_skipped_lines)`.
    pub fn poll(&mut self, path: &Path) -> (Vec<crate::usage::recorder::UsageEntry>, usize) {
        let Ok(meta) = fs::metadata(path) else {
            return (Vec::new(), 0);
        };
        let len = meta.len();
        if len < self.offset {
            // Truncated/rewritten — rescan everything.
            return self.load_all(path);
        }
        if len == self.offset {
            return (Vec::new(), 0);
        }
        let Ok(mut f) = OpenOptions::new().read(true).open(path) else {
            return (Vec::new(), 0);
        };
        use std::io::Seek;
        use std::io::SeekFrom;
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            // Unreadable mid-read — rescan from the top to stay consistent.
            return self.load_all(path);
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_err() {
            return self.load_all(path);
        }
        self.offset = len;

        let mut text = std::mem::take(&mut self.pending);
        text.push_str(&buf);

        let mut entries = Vec::new();
        let mut skipped = 0;
        // Keep the remainder after the LAST newline as the pending tail.
        if let Some(last_nl) = text.rfind('\n') {
            self.pending = text[last_nl + 1..].to_string();
            text.truncate(last_nl + 1);
        } else {
            // No newline at all: the whole thing is still being written.
            self.pending = text;
            return (Vec::new(), 0);
        }
        for line in text.lines() {
            if line.trim().is_empty() {
                continue; // blank lines are not "bad" lines
            }
            match parse_line(line) {
                Some(e) => entries.push(e),
                None => skipped += 1,
            }
        }
        self.skipped += skipped;
        (entries, skipped)
    }
}

#[cfg(test)]
mod tests;
