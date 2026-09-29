//! 2a: incremental reader for the append-only per-agent metrics JSONL.
//!
//! Ports the `UsageLogReader` pattern (`usage/stats.rs`): byte-offset
//! polling so a UI can pick up only newly appended lines, a buffered torn
//! trailing line (a write in progress), and a transparent full rescan when
//! the file shrank (truncated/rotated).

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::MetricsLine;

/// Incremental reader for one agent's metrics JSONL file.
///
/// Tracks the byte offset already read so `poll` returns only new, fully
/// written lines. If the file shrank (truncated or rewritten), the next
/// `poll` transparently falls back to a full rescan. An incomplete trailing
/// line (torn write in progress) is buffered until the newline arrives.
#[derive(Default, Debug)]
pub struct MetricsLogReader {
    offset: u64,
    /// Incomplete trailing line from the previous read, if any.
    pending: String,
    /// Malformed lines skipped since the last full load.
    skipped: usize,
}

impl MetricsLogReader {
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

    /// Read the entire file, replacing any previously read state (oldest
    /// first). Corrupt lines are skipped (debug-logged), like
    /// `MetricsLog::read_all`.
    pub fn load_all(&mut self, path: &Path) -> Vec<MetricsLine> {
        self.reset();
        let (lines, skipped) = read_lines(path);
        self.skipped = skipped;
        if let Ok(meta) = fs::metadata(path) {
            self.offset = meta.len();
        }
        lines
    }

    /// Read only the bytes appended since the last call.
    ///
    /// - File missing or unchanged: `[]`.
    /// - File shrank below the tracked offset: full rescan (returns ALL
    ///   lines, like `load_all`).
    /// - A partial trailing line (no newline yet) is buffered, not skipped.
    ///
    /// Returns the newly appended, fully written lines (oldest first).
    pub fn poll(&mut self, path: &Path) -> Vec<MetricsLine> {
        let Ok(meta) = fs::metadata(path) else {
            return Vec::new();
        };
        let len = meta.len();
        if len < self.offset {
            // Truncated/rewritten — rescan everything.
            return self.load_all(path);
        }
        if len == self.offset {
            return Vec::new();
        }
        let Ok(mut f) = OpenOptions::new().read(true).open(path) else {
            return Vec::new();
        };
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

        // Keep the remainder after the LAST newline as the pending tail.
        let Some(last_nl) = text.rfind('\n') else {
            // No newline at all: the whole thing is still being written.
            self.pending = text;
            return Vec::new();
        };
        self.pending = text[last_nl + 1..].to_string();
        text.truncate(last_nl + 1);

        let mut lines = Vec::new();
        let mut skipped = 0;
        for raw in text.lines() {
            let raw = raw.trim();
            if raw.is_empty() {
                continue; // blank lines are not "bad" lines
            }
            match serde_json::from_str::<MetricsLine>(raw) {
                Ok(l) => lines.push(l),
                Err(e) => {
                    tracing::debug!("skipping corrupt metrics line in {:?}: {e}", path);
                    skipped += 1;
                }
            }
        }
        self.skipped += skipped;
        lines
    }
}

/// Parse every line of `path` (oldest first), skipping blank/corrupt lines.
fn read_lines(path: &Path) -> (Vec<MetricsLine>, usize) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (Vec::new(), 0),
    };
    let mut lines = Vec::new();
    let mut skipped = 0;
    for raw in content.lines() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<MetricsLine>(raw) {
            Ok(l) => lines.push(l),
            Err(e) => {
                tracing::debug!("skipping corrupt metrics line in {:?}: {e}", path);
                skipped += 1;
            }
        }
    }
    (lines, skipped)
}
