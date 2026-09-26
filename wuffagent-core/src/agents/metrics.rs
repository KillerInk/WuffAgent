//! M1: per-agent run metrics — append-only JSONL, one file per agent.
//!
//! Each line lives in `<wuffagent_home>/metrics/<agent>.jsonl`:
//! ```json
//! {"kind":"run","ts":"2026-09-25T13:44:15.123Z","tool_calls":12,"tool_errors":2,"verification_attempts":1,"duration_ms":45210,"outcome":"verified"}
//! {"kind":"feedback","ts":"2026-09-25T13:50:00.000Z","feedback":"up"}
//! ```
//!
//! Two line kinds, one file per agent (the file name IS the agent name, so
//! the line carries no agent field):
//! - `run` — one completed agent LLM-loop (written at `run_llm_loop` end;
//!   each handoff hop records its own line under its own agent).
//! - `feedback` — a 👍/👎 the user gave on an assistant answer
//!   (written by the chat feedback path, independent of the memory store).
//!
//! Terminal run `outcome` values: `verified` (first-try pass, including the
//! no-tool-outputs shortcut and verification-LLM-error default-pass),
//! `verified_after_retry` (the judge failed the first attempt, the nudged
//! retry passed), `gave_up` (the nudge budget was exhausted), `none` (the run
//! ended without completing verification — handoff/restart/hand-back). There
//! is no `needs_fix` terminal state: the nudge loop turns a failed verdict
//! into a retry until it passes or gives up.
//!
//! Writes are best-effort (a broken log must never break the run) and reads
//! are tolerant (corrupt lines are skipped), mirroring `usage/recorder.rs`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Terminal verification outcome of an agent run (see the module docs for the
/// exact semantics of each value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Verified,
    VerifiedAfterRetry,
    GaveUp,
    /// The run ended before completing verification (handoff/restart/…).
    None,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::VerifiedAfterRetry => "verified_after_retry",
            Self::GaveUp => "gave_up",
            Self::None => "none",
        }
    }
}

/// User rating on an assistant answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackKind {
    Up,
    Down,
}

/// One line in a per-agent metrics log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricsLine {
    /// One completed agent run.
    Run {
        /// UTC timestamp of the run's end.
        ts: DateTime<Utc>,
        /// Tool calls executed in the run.
        tool_calls: u32,
        /// Tool calls that returned an error.
        tool_errors: u32,
        /// Verification judge attempts used (0 = none).
        verification_attempts: u32,
        /// Wall-clock duration of the LLM loop, in milliseconds.
        duration_ms: u64,
        /// Terminal verification outcome.
        outcome: RunOutcome,
    },
    /// User feedback on an assistant answer.
    Feedback {
        /// UTC timestamp of the rating.
        ts: DateTime<Utc>,
        feedback: FeedbackKind,
    },
}

impl MetricsLine {
    /// One-line human/LLM-readable rendering (newest-first lists in the UI,
    /// the improver prompt).
    pub fn describe(&self) -> String {
        match self {
            MetricsLine::Run {
                ts,
                tool_calls,
                tool_errors,
                verification_attempts,
                duration_ms,
                outcome,
            } => format!(
                "{} run: {} tool calls ({} errors), {} verification attempt(s), {:.1}s, outcome: {}",
                ts.format("%Y-%m-%d %H:%M"),
                tool_calls,
                tool_errors,
                verification_attempts,
                *duration_ms as f64 / 1000.0,
                outcome.as_str(),
            ),
            MetricsLine::Feedback { ts, feedback } => format!(
                "{} feedback: {}",
                ts.format("%Y-%m-%d %H:%M"),
                if *feedback == FeedbackKind::Up { "up" } else { "down" }
            ),
        }
    }
}

/// Aggregate counts over an agent's metric lines (all-time or since `since`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsSummary {
    pub runs: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub verified: u32,
    pub verified_after_retry: u32,
    pub gave_up: u32,
    /// Runs that ended without completing verification (handoff/restart/…).
    pub not_verified: u32,
    pub feedback_up: u32,
    pub feedback_down: u32,
}

impl MetricsSummary {
    /// One-line rendering with a caller-supplied label (the I5 effect check
    /// labels its before/after windows). Returns an empty string when there
    /// is nothing to report (no runs and no feedback).
    pub fn format_labeled(&self, label: &str) -> String {
        if self.runs == 0 && self.feedback_up == 0 && self.feedback_down == 0 {
            return String::new();
        }
        let error_rate = if self.tool_calls > 0 {
            100.0 * self.tool_errors as f64 / self.tool_calls as f64
        } else {
            0.0
        };
        format!(
            "{label} ({} run(s), {} tool call(s) with {} errors ({:.1}%), \
             outcomes: {} verified / {} verified_after_retry / {} gave_up / {} not verified, \
             user feedback: {} up / {} down)",
            self.runs,
            self.tool_calls,
            self.tool_errors,
            error_rate,
            self.verified,
            self.verified_after_retry,
            self.gave_up,
            self.not_verified,
            self.feedback_up,
            self.feedback_down,
        )
    }

    /// One-line rendering for the improver prompt ("Recent metrics: …").
    pub fn format_line(&self) -> String {
        self.format_labeled("Recent metrics")
    }
}

/// File name for an agent's metrics log: lowercased, `[a-z0-9_-]` only,
/// everything else replaced by `-` (runs of `-` collapsed, leading/trailing
/// `-` trimmed). Empty → `agent`.
pub fn agent_file_name(agent: &str) -> String {
    let s: String = agent
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut out = String::new();
    for c in s.chars() {
        if c == '-' && out.ends_with('-') {
            continue; // collapse runs
        }
        out.push(c);
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "agent".to_string()
    } else {
        out
    }
}

/// Test override for the default metrics directory (the improver's tests
/// must not read the real `~/.wuffagent/metrics`). Same process-global
/// pattern as `config::set_config_path_for_testing`; tests that use it
/// serialize on their own lock (see `MetricsDirGuard` in the improvement
/// tests).
static TEST_METRICS_DIR: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn test_metrics_dir_slot() -> &'static std::sync::Mutex<Option<PathBuf>> {
    TEST_METRICS_DIR.get_or_init(|| std::sync::Mutex::new(None))
}

fn test_metrics_dir() -> Option<PathBuf> {
    test_metrics_dir_slot().lock().unwrap().clone()
}

/// Set (or clear with `None`) the test override for the default metrics dir.
pub fn set_metrics_dir_for_testing(dir: Option<PathBuf>) {
    *test_metrics_dir_slot().lock().unwrap() = dir;
}

/// Per-process temp dir that [`MetricsLog::default`] falls back to when this
/// crate runs under `#[cfg(test)]` (its own test binary only — a dependent
/// crate's test binary still sees the real location and must set
/// `set_metrics_dir_for_testing` explicitly in its tests).
#[cfg(test)]
fn test_process_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::temp_dir().join(format!("wuffagent-metrics-test-{}", std::process::id()))
    })
    .clone()
}

/// Per-agent metrics log (append-only JSONL, one file per agent).
///
/// Construction is side-effect free (no I/O); each write opens its file in
/// append mode, so a fresh process never holds stale handles and clones are
/// trivially correct.
pub struct MetricsLog {
    dir: PathBuf,
    /// Set after the first write failure so repeated failures log at
    /// `debug!` instead of warning on every run.
    warned: AtomicBool,
}

impl MetricsLog {
    /// Create a log rooted at `dir` (files: `<dir>/<agent>.jsonl`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            warned: AtomicBool::new(false),
        }
    }

    /// Default location: `~/.wuffagent/metrics/`, sibling of `sessions/` and
    /// `usage.jsonl` (see `config::get_wuffagent_home`). An explicit test
    /// override wins when set; inside THIS crate's test binary the fallback
    /// is a per-process temp dir (see `test_process_dir`), so tests that
    /// exercise production paths (the run writer, the improver reader) never
    /// pollute the real metrics dir.
    pub fn default() -> Self {
        if let Some(p) = test_metrics_dir() {
            Self::new(p)
        } else {
            #[cfg(test)]
            {
                Self::new(test_process_dir())
            }
            #[cfg(not(test))]
            {
                Self::new(crate::config::get_wuffagent_home().join("metrics"))
            }
        }
    }

    /// The metrics directory this log writes into.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The JSONL file for one agent.
    pub fn agent_path(&self, agent: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", agent_file_name(agent)))
    }

    /// Append one line. Best-effort: all failures are reported via
    /// `tracing` and swallowed (a broken log must never break the run).
    pub fn append(&self, agent: &str, line: &MetricsLine) {
        let text = match serde_json::to_string(line) {
            Ok(t) => t,
            Err(e) => {
                self.report_failure(&format!("serialize metrics line: {e}"));
                return;
            }
        };
        let path = self.agent_path(agent);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                self.report_failure(&format!("create metrics dir {:?}: {e}", parent));
                return;
            }
        }
        let mut file = match OpenOptions::new().append(true).create(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                self.report_failure(&format!("open metrics log {:?}: {e}", path));
                return;
            }
        };
        if let Err(e) = writeln!(file, "{text}").and_then(|_| file.flush()) {
            self.report_failure(&format!("write metrics log {:?}: {e}", path));
        }
    }

    /// Append a completed-run line for `agent`.
    pub fn log_run(
        &self,
        agent: &str,
        tool_calls: u32,
        tool_errors: u32,
        verification_attempts: u32,
        duration_ms: u64,
        outcome: RunOutcome,
    ) {
        self.append(
            agent,
            &MetricsLine::Run {
                ts: Utc::now(),
                tool_calls,
                tool_errors,
                verification_attempts,
                duration_ms,
                outcome,
            },
        );
    }

    /// Append a user-feedback line for `agent`.
    pub fn log_feedback(&self, agent: &str, up: bool) {
        self.append(
            agent,
            &MetricsLine::Feedback {
                ts: Utc::now(),
                feedback: if up {
                    FeedbackKind::Up
                } else {
                    FeedbackKind::Down
                },
            },
        );
    }

    /// Read all lines for `agent` (oldest first), skipping corrupt lines.
    /// Returns an empty vec when the file does not exist.
    pub fn read_all(&self, agent: &str) -> Vec<MetricsLine> {
        let path = self.agent_path(agent);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut lines = Vec::new();
        for raw in content.lines() {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            match serde_json::from_str::<MetricsLine>(raw) {
                Ok(l) => lines.push(l),
                Err(e) => tracing::debug!("skipping corrupt metrics line in {:?}: {e}", path),
            }
        }
        lines
    }

    /// The most recent `n` lines for `agent` (oldest first).
    pub fn recent(&self, agent: &str, n: usize) -> Vec<MetricsLine> {
        let all = self.read_all(agent);
        let skip = all.len().saturating_sub(n);
        all.into_iter().skip(skip).collect()
    }

    /// All agents that have a metrics file (file names without the `.jsonl`
    /// suffix, sorted). For the fleet summary — the cross-agent comparison
    /// the improver uses to see one agent's results next to its siblings'.
    pub fn agent_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return names;
        };
        for entry in entries.flatten() {
            if let Some(stem) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".jsonl"))
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
        names
    }

    /// Aggregate counts for `agent` over lines with `ts >= since`
    /// (`None` = all time).
    pub fn summary_since(&self, agent: &str, since: Option<DateTime<Utc>>) -> MetricsSummary {
        self.summary_between(agent, since, None)
    }

    /// Aggregate counts for `agent` over lines with `start <= ts < end`
    /// (a `None` bound is unbounded; `start >= end` yields the empty
    /// summary). The single code path behind `summary_since` (end = `None`)
    /// and the I5 effect-check before/after windows.
    pub fn summary_between(
        &self,
        agent: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> MetricsSummary {
        let mut s = MetricsSummary::default();
        if let (Some(start), Some(end)) = (start, end) {
            if start >= end {
                return s;
            }
        }
        for line in self.read_all(agent) {
            let ts = match &line {
                MetricsLine::Run { ts, .. } => ts,
                MetricsLine::Feedback { ts, .. } => ts,
            };
            if let Some(start) = start {
                if *ts < start {
                    continue;
                }
            }
            if let Some(end) = end {
                if *ts >= end {
                    continue;
                }
            }
            match &line {
                MetricsLine::Run {
                    tool_calls,
                    tool_errors,
                    outcome,
                    ..
                } => {
                    s.runs += 1;
                    s.tool_calls += tool_calls;
                    s.tool_errors += tool_errors;
                    match outcome {
                        RunOutcome::Verified => s.verified += 1,
                        RunOutcome::VerifiedAfterRetry => s.verified_after_retry += 1,
                        RunOutcome::GaveUp => s.gave_up += 1,
                        RunOutcome::None => s.not_verified += 1,
                    }
                }
                MetricsLine::Feedback { feedback, .. } => match feedback {
                    FeedbackKind::Up => s.feedback_up += 1,
                    FeedbackKind::Down => s.feedback_down += 1,
                },
            }
        }
        s
    }

    fn report_failure(&self, msg: &str) {
        if self.warned.swap(true, Ordering::Relaxed) {
            tracing::debug!("{msg}");
        } else {
            tracing::warn!("{msg}");
        }
    }
}

/// Record a completed run in the DEFAULT metrics log (writer hook for
/// `run_llm_loop`). Best-effort — never fails the run.
pub fn record_run(
    agent: &str,
    tool_calls: u32,
    tool_errors: u32,
    verification_attempts: u32,
    duration_ms: u64,
    outcome: RunOutcome,
) {
    MetricsLog::default().log_run(
        agent,
        tool_calls,
        tool_errors,
        verification_attempts,
        duration_ms,
        outcome,
    );
}

/// Record user feedback in the DEFAULT metrics log (writer hook for the chat
/// feedback path). Best-effort — independent of the memory store.
pub fn record_feedback(agent: &str, up: bool) {
    MetricsLog::default().log_feedback(agent, up);
}

#[cfg(test)]
mod tests;
