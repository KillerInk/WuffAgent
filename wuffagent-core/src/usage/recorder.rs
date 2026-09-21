//! Appender-only JSONL log of completed LLM calls.
//!
//! One line per call:
//! ```json
//! {"ts":"2026-09-21T12:34:56.789Z","session_id":"…","agent":"general","model":"deepseek-chat","prompt_tokens":12345,"completion_tokens":678,"total_tokens":13023,"tool_calls":2,"thinking_chars":1543}
//! ```
//!
//! `tool_calls` / `thinking_chars` are absent in lines written before they
//! existed; deserialization defaults them to 0.
//!
//! Writes are best-effort: serialization or I/O failures are reported via
//! `tracing::warn!` (at most once per failure mode, then `debug!`) and never
//! propagate — a broken log file must not break the chat.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One completed LLM call, as stored in the JSONL log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageEntry {
    /// UTC timestamp of the completed call (millisecond precision).
    pub ts: DateTime<Utc>,
    /// Session the call belonged to (empty when unset).
    pub session_id: String,
    /// Agent name that made the call (e.g. `general`, `coder`, `chat`).
    pub agent: String,
    /// Model reported by the server (`unknown` when it didn't say).
    pub model: String,
    /// Input tokens (server-reported).
    pub prompt_tokens: u32,
    /// Output tokens (server-reported).
    pub completion_tokens: u32,
    /// Total tokens (server-reported).
    pub total_tokens: u32,
    /// Number of tool calls the assistant issued in this call (0 = none).
    /// Absent in pre-existing log lines (serde default 0).
    #[serde(default)]
    pub tool_calls: u32,
    /// Character count of the model's thinking/reasoning text in this call
    /// (0 = none). Absent in pre-existing log lines (serde default 0).
    #[serde(default)]
    pub thinking_chars: u64,
}

/// Appends [`UsageEntry`] lines to a JSONL file.
///
/// Construction is side-effect free (no I/O); the file is opened lazily on
/// the first `record`, so building a recorder in unit tests never touches
/// the filesystem.
pub struct UsageRecorder {
    path: PathBuf,
    file: Mutex<Option<std::io::BufWriter<File>>>,
    /// Set after the first write failure so repeated failures log at
    /// `debug!` instead of warning on every LLM call.
    warned: AtomicBool,
}

// `Mutex<Option<BufWriter<File>>>` and `AtomicBool` are not `Clone`, but a
// clone is cheap and correct: the new handle re-opens the file (append mode)
// on its first write.
impl Clone for UsageRecorder {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            file: Mutex::new(None),
            warned: AtomicBool::new(self.warned.load(Ordering::Relaxed)),
        }
    }
}

impl UsageRecorder {
    /// Create a recorder for `path`. The file is created (and its parent
    /// directory, if needed) on the first successful write.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file: Mutex::new(None),
            warned: AtomicBool::new(false),
        }
    }

    /// Default log location: `~/.wuffagent/usage.jsonl`, sibling of
    /// `sessions/` (see `config::get_wuffagent_home`).
    pub fn default_path() -> PathBuf {
        crate::config::get_wuffagent_home().join("usage.jsonl")
    }

    /// Recorder at the default log location.
    pub fn default_recorder() -> Self {
        Self::new(Self::default_path())
    }

    /// The log file path this recorder appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry as a JSON line. Best-effort: all failures are
    /// reported via `tracing` and swallowed.
    pub fn record(&self, entry: &UsageEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(e) => {
                self.report_failure(&format!("serialize usage entry: {e}"));
                return;
            }
        };
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_none() {
            // The default home dir normally exists; create it anyway so a
            // freshly-provisioned profile (or a test path) works.
            if let Some(parent) = self.path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    self.report_failure(&format!(
                        "create usage log dir {:?}: {e}",
                        parent
                    ));
                    return;
                }
            }
            let mut opts = OpenOptions::new();
            opts.append(true).create(true);
            match opts.open(&self.path) {
                Ok(f) => *guard = Some(std::io::BufWriter::new(f)),
                Err(e) => {
                    self.report_failure(&format!(
                        "open usage log {:?}: {e}",
                        self.path
                    ));
                    return;
                }
            }
        }
        let Some(file) = guard.as_mut() else {
            return;
        };
        if let Err(e) = writeln!(file, "{line}").and_then(|_| file.flush()) {
            self.report_failure(&format!("write usage log {:?}: {e}", self.path));
        }
    }

    fn report_failure(&self, msg: &str) {
        if self.warned.swap(true, Ordering::Relaxed) {
            tracing::debug!("{msg}");
        } else {
            tracing::warn!("{msg}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_path(name: &str) -> PathBuf {
        let nix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("wuffagent-usage-{}-{}-{nix}", std::process::id(), name))
    }

    fn entry(total: u32) -> UsageEntry {
        UsageEntry {
            ts: Utc.with_ymd_and_hms(2026, 9, 21, 12, 34, 56).unwrap(),
            session_id: "sess-1".to_string(),
            agent: "general".to_string(),
            model: "test-model".to_string(),
            prompt_tokens: total.saturating_sub(total / 10),
            completion_tokens: total / 10,
            total_tokens: total,
            // Non-zero so the round-trip test actually covers the fields.
            tool_calls: (total / 100).max(1),
            thinking_chars: (total as u64) * 7,
        }
    }

    #[test]
    fn records_roundtrip_and_append() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let rec = UsageRecorder::new(&path);
        rec.record(&entry(100));
        rec.record(&entry(200));

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "one line per record: {content}");
        let parsed: UsageEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.total_tokens, 100);
        assert_eq!(parsed.session_id, "sess-1");
        let parsed2: UsageEntry = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(parsed2.total_tokens, 200);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unopenable_path_never_panics() {
        // A "directory" that is actually a file: create_dir_all succeeds (it
        // exists), but opening the log inside it fails — the recorder must
        // degrade to a warn, not fail the caller.
        let base = std::env::temp_dir().join(format!(
            "wuffagent-usage-blocked-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&base);
        std::fs::write(&base, b"blocker").unwrap();
        let rec = UsageRecorder::new(base.join("usage.jsonl"));
        rec.record(&entry(1)); // must not panic
        rec.record(&entry(2)); // repeated failure must not panic either
        let _ = std::fs::remove_file(&base);
    }
}
