//! M1: `MetricsLog` — the append-only per-agent JSONL store: file layout,
//! best-effort appends (`log_*` + the free `record_*` shorthands), tolerant
//! reads, and the 4b daily rollup/prune. The process-global test-dir
//! override helpers live here too (shared with the improvement tests).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, NaiveDate, Utc};

use crate::agents::types::RunStats;
use crate::stats::bucket::Granularity;

use super::aggregates::*;
use super::schema::*;

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

/// Serializes the tests that read or write the process-global test override
/// above: it is one global slot, so a test asserting the DEFAULT dir
/// (`test_default_uses_test_process_dir`) races with any parallel test that
/// sets an override. Every such test — the hygiene tests in this module AND
/// the `MetricsDirGuard` users in the improvement tests — must hold this
/// lock for the whole test body.
#[cfg(test)]
pub(crate) static METRICS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Per-process temp dir that [`MetricsLog::default`] falls back to when this
/// crate runs under `#[cfg(test)]` (its own test binary only — a dependent
/// crate's test binary still sees the real location and must set
/// `set_metrics_dir_for_testing` explicitly in its tests).
#[cfg(test)]
pub(crate) fn test_process_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::temp_dir().join(format!("wuffagent-metrics-test-{}", std::process::id()))
    })
    .clone()
}

/// Reserved file (without the `.jsonl` suffix) for cross-agent `skill_use`
/// lines — skills are shared across agents and the read_skill tool has no
/// per-agent context, so usage is recorded fleet-wide.
pub const SKILLS_FILE_STEM: &str = "skills";

/// Reserved file (without the `.jsonl` suffix) for fleet-wide `check` lines
/// (the self-improvement loop's own cost when reviewing the whole fleet at
/// once). Like the skills file, it is not an agent, so `agent_names()` skips
/// it.
pub const FLEET_FILE_STEM: &str = "fleet";

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

    /// Append a completed-run line for `agent` (1a: the full RunStats, so
    /// the per-tool histogram lands on the line alongside the scalars).
    pub fn log_run(
        &self,
        agent: &str,
        stats: &RunStats,
        duration_ms: u64,
        outcome: RunOutcome,
        tokens_in: u64,
        tokens_out: u64,
        run_id: &str,
        session_id: &str,
    ) {
        self.append(
            agent,
            &MetricsLine::Run {
                ts: Utc::now(),
                tool_calls: stats.tool_calls as u32,
                tool_errors: stats.tool_errors as u32,
                verification_attempts: stats.verification_attempts,
                duration_ms,
                outcome,
                tokens_in,
                tokens_out,
                tools: stats.tools.clone(),
                run_id: run_id.to_string(),
                session_id: session_id.to_string(),
                llm_ms: stats.llm_ms,
                tools_ms: stats.tools.iter().map(|t| t.duration_ms).sum(),
                model: stats.model.clone(),
                cost_usd: stats.cost_usd,
                v: 1,
            },
        );
    }

    /// Append a skill-usage line to the reserved cross-agent `skills.jsonl`
    /// file (see [`SKILLS_FILE_STEM`]).
    pub fn log_skill_use(&self, skill: &str) {
        self.append(
            SKILLS_FILE_STEM,
            &MetricsLine::SkillUse {
                ts: Utc::now(),
                skill: skill.to_string(),
                v: 1,
            },
        );
    }

    /// Append a context-trim line for `agent` (the context-rot signal).
    pub fn log_trim(
        &self,
        agent: &str,
        chars_before: u64,
        chars_after: u64,
        messages_removed: u32,
        brief_updated: bool,
        overflow: bool,
        run_id: &str,
    ) {
        self.append(
            agent,
            &MetricsLine::Trim {
                ts: Utc::now(),
                chars_before,
                chars_after,
                messages_removed,
                brief_updated,
                overflow,
                run_id: run_id.to_string(),
                v: 1,
            },
        );
    }

    /// Append a user-feedback line for `agent` (message-level: no run link).
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
                run_id: None,
                v: 1,
            },
        );
    }

    /// 3d: append a RUN-LEVEL user-feedback line for `agent`, linked to the
    /// specific run `run_id` (the agent editor's 👍/👎 on a recent-run row).
    /// `run_id` is the Run line's 1e join key.
    pub fn log_feedback_run(&self, agent: &str, run_id: &str, up: bool) {
        self.append(
            agent,
            &MetricsLine::Feedback {
                ts: Utc::now(),
                feedback: if up {
                    FeedbackKind::Up
                } else {
                    FeedbackKind::Down
                },
                run_id: Some(run_id.to_string()),
                v: 1,
            },
        );
    }

    /// Append a self-improvement-check line (the loop's own cost, 1c). The
    /// line is written to `agent`'s file — for scope "agent" that is the
    /// reviewed profile's file, for scope "fleet" the caller passes the
    /// reserved "fleet" stem (so it lands in `fleet.jsonl`).
    pub fn log_check(
        &self,
        agent: &str,
        scope: &str,
        tokens_in: u64,
        tokens_out: u64,
        suggestions: usize,
        duration_ms: u64,
    ) {
        self.append(
            agent,
            &MetricsLine::Check {
                ts: Utc::now(),
                agent: agent.to_string(),
                scope: scope.to_string(),
                tokens_in,
                tokens_out,
                suggestions,
                duration_ms,
                v: 1,
            },
        );
    }

    /// 2b: append a golden/regression eval line (the eval harness's pass/fail
    /// record). Written to `agent`'s file (the profile that ran the eval).
    /// 1c: `score` is the judge's 0..=1 quality score (None = the judge gave
    /// no usable score line, or the run never reached the judge).
    pub fn log_eval(
        &self,
        agent: &str,
        id: &str,
        passed: bool,
        score: Option<f64>,
        duration_ms: u64,
        tokens_in: u64,
        tokens_out: u64,
        model: &str,
        cost_usd: f64,
    ) {
        self.append(
            agent,
            &MetricsLine::Eval {
                ts: Utc::now(),
                agent: agent.to_string(),
                id: id.to_string(),
                passed,
                score,
                duration_ms,
                tokens_in,
                tokens_out,
                model: model.to_string(),
                cost_usd,
                v: 1,
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
                if stem == SKILLS_FILE_STEM || stem == FLEET_FILE_STEM {
                    continue; // reserved cross-agent file, not an agent
                }
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

    /// 2b: zero-filled time-bucket window over `agent`'s metric lines,
    /// using the shared usage-store bucket math (`stats/bucket.rs`).
    ///
    /// `granularity` selects the window size (Hour=24, Day=30, Week=12
    /// buckets) ending at `now` (UTC); non-Run lines are ignored. Returned
    /// oldest first, `buckets.len() == granularity.bucket_count()`.
    pub fn bucket_summary(
        &self,
        agent: &str,
        granularity: Granularity,
        now: DateTime<Utc>,
    ) -> Vec<BucketSummary> {
        // 3b: the bucketing lives in the pure `bucket_summary_from_lines`
        // (shared with the egui agent editor's cached vec) — this method is
        // just the file read + delegate.
        bucket_summary_from_lines(&self.read_all(agent), granularity, now)
    }

    /// 2c: full report over the window `ts >= since && ts < end`
    /// (`None` bounds = unbounded) — the aggregate summary plus the
    /// per-run percentiles and the tool/model breakdowns. 4b: rolled-up
    /// days (their raw run lines pruned by retention) are merged in, so
    /// pruning never loses an aggregate this method reports.
    pub fn report(
        &self,
        agent: &str,
        since: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> MetricsReport {
        // 3b: the computation lives in the pure `build_report` (shared with
        // the egui agent editor's cached vec via `metrics_report_from_lines`)
        // — this method is the file read + rollup merge + delegate.
        let lines = self.read_all(agent);
        let rt = self.load_rollups_in(agent, since, end, &lines);
        build_report(&lines, since, end, &rt)
    }

    /// 4a: join one run across the metrics + usage stores by the 1e
    /// `run_id` — the run's own line, its LLM rounds (usage.jsonl entries
    /// with the same run_id), and its run-level feedback. `None` when no
    /// run line for `agent` carries that id. Reads both JSONL files in full
    /// (on-demand; never called per frame).
    pub fn run_detail(&self, agent: &str, run_id: &str) -> Option<RunDetail> {
        let lines = self.read_all(agent);
        let run = lines
            .iter()
            .rev()
            .find(|l| matches!(l, MetricsLine::Run { run_id: rid, .. } if rid == run_id))?
            .clone();
        let feedback = lines
            .iter()
            .filter(|l| {
                matches!(l, MetricsLine::Feedback { run_id: Some(frid), .. } if frid == run_id)
            })
            .cloned()
            .collect();
        let (entries, _) = crate::usage::stats::load_entries(
            &crate::usage::recorder::UsageRecorder::usage_log_path(),
        );
        let rounds = entries
            .into_iter()
            .filter(|e| e.run_id == run_id)
            .collect();
        Some(RunDetail {
            run,
            rounds,
            feedback,
        })
    }

    /// 4b: path of the rollup file for one agent's UTC day
    /// (`metrics/rollups/<agent_file>-YYYY-MM-DD.json`).
    pub fn rollup_path(&self, agent: &str, day: NaiveDate) -> std::path::PathBuf {
        self.dir
            .join("rollups")
            .join(format!("{}-{}.json", agent_file_name(agent), day.format("%Y-%m-%d")))
    }

    /// 4b: the agent's rollup totals for the window `ts >= since && ts <
    /// end` — every rollup file whose day is (a) fully inside the window
    /// and (b) entirely before the raw file's oldest run line (otherwise
    /// the raw file still holds that day's runs and would double-count).
    /// Corrupt rollup files are skipped (tolerant, like `read_all`).
    fn load_rollups_in(
        &self,
        agent: &str,
        since: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        raw_lines: &[MetricsLine],
    ) -> RollupTotals {
        let mut totals = RollupTotals::default();
        let raw_min_run_ts = raw_lines
            .iter()
            .filter_map(|l| match l {
                MetricsLine::Run { ts, .. } => Some(*ts),
                _ => None,
            })
            .min();
        let rollups_dir = self.dir.join("rollups");
        let Ok(entries) = std::fs::read_dir(&rollups_dir) else {
            return totals;
        };
        let prefix = format!("{}-", agent_file_name(agent));
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(date_str) = name.strip_prefix(&prefix).and_then(|s| s.strip_suffix(".json"))
            else {
                continue;
            };
            let Ok(day) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
                continue;
            };
            let day_start = day
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            let day_end = (day + chrono::Duration::days(1))
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            if let Some(s) = since {
                if day_start < s {
                    continue;
                }
            }
            if let Some(e) = end {
                if day_end > e {
                    continue;
                }
            }
            if let Some(min) = raw_min_run_ts {
                if day_end > min {
                    continue;
                }
            }
            match std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|content| serde_json::from_str::<MetricsRollup>(&content).ok())
            {
                Some(r) => totals.add_rollup(&r),
                None => {
                    tracing::debug!(
                        "skipping unreadable/corrupt metrics rollup {}",
                        entry.path().display()
                    );
                }
            }
        }
        totals
    }

    /// 4b: retention rotation for one agent — write a rollup file for every
    /// fully-elapsed UTC day that is entirely older than `retention_days`
    /// and has no rollup yet, then prune the raw `run` lines of the days
    /// that now HAVE a rollup. Non-run lines (feedback/skill_use/trim/check/
    /// eval) are never pruned — small and high-signal. A day is only pruned
    /// if its rollup file exists on disk, so a failed write never loses
    /// data. Returns the rollup paths written (empty = nothing to do).
    pub fn roll_up_and_prune(
        &self,
        agent: &str,
        now: DateTime<Utc>,
        retention_days: u32,
    ) -> Vec<std::path::PathBuf> {
        let lines = self.read_all(agent);
        if lines.is_empty() {
            return Vec::new();
        }
        let cutoff = now - chrono::Duration::days(retention_days as i64);
        // A day is eligible when it has fully elapsed AND is entirely
        // before the retention cutoff.
        let eligible_before = now.date_naive().min(cutoff.date_naive());

        // Group the agent's run lines by UTC day.
        let mut day_runs: std::collections::BTreeMap<NaiveDate, Vec<&MetricsLine>> =
            std::collections::BTreeMap::new();
        for line in &lines {
            if let MetricsLine::Run { ts, .. } = line {
                day_runs.entry(ts.date_naive()).or_default().push(line);
            }
        }
        // Trim correlation for a day links via the 1e run_id: the trimmed
        // ids are the non-empty run_ids of the agent's trim lines.
        let trimmed_ids: std::collections::HashSet<&str> = lines
            .iter()
            .filter_map(|l| match l {
                MetricsLine::Trim { run_id, .. } if !run_id.is_empty() => Some(run_id.as_str()),
                _ => None,
            })
            .collect();

        let mut written = Vec::new();
        for (day, runs) in day_runs.iter().filter(|(d, _)| **d < eligible_before) {
            let path = self.rollup_path(agent, *day);
            if path.exists() {
                continue; // already rolled up (idempotent across restarts)
            }
            let mut rollup = MetricsRollup {
                day: day.format("%Y-%m-%d").to_string(),
                agent: agent.to_string(),
                summary: MetricsSummary::from_lines(
                    &runs.iter().map(|l| (*l).clone()).collect::<Vec<_>>(),
                ),
                ..Default::default()
            };
            for line in runs {
                if let MetricsLine::Run {
                    cost_usd,
                    model,
                    outcome,
                    run_id,
                    ..
                } = line
                {
                    rollup.cost_usd += cost_usd;
                    // Same "(unknown)" convention as build_report.
                    let key = if model.trim().is_empty() {
                        "(unknown)"
                    } else {
                        model.trim()
                    };
                    *rollup.model_mix.entry(key.to_string()).or_insert(0) += 1;
                    let gave_up = matches!(outcome, RunOutcome::GaveUp);
                    if trimmed_ids.contains(run_id.as_str()) {
                        rollup.trim_correlation.trimmed_runs += 1;
                        if gave_up {
                            rollup.trim_correlation.trimmed_gave_up += 1;
                        }
                    } else {
                        rollup.trim_correlation.untrimmed_runs += 1;
                        if gave_up {
                            rollup.trim_correlation.untrimmed_gave_up += 1;
                        }
                    }
                    // Per-tool totals (the additive raw form the report
                    // turns into ReportToolStat).
                    if let MetricsLine::Run { tools, .. } = line {
                        for t in tools {
                            let e = rollup
                                .tools
                                .entry(t.name.clone())
                                .or_insert((0, 0, 0));
                            e.0 += t.calls;
                            e.1 += t.errors;
                            e.2 += t.duration_ms;
                        }
                    }
                }
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match serde_json::to_string(&rollup) {
                Ok(json) => {
                    if std::fs::write(&path, json).is_ok() {
                        written.push(path);
                    } else {
                        tracing::warn!(
                            "4b: failed writing metrics rollup {}",
                            path.display()
                        );
                    }
                }
                Err(e) => tracing::warn!("4b: failed serializing rollup: {e}"),
            }
        }
        if !written.is_empty() {
            // Prune ONLY the run lines of days whose rollup now exists on
            // disk (a failed write keeps its raw lines).
            let n = lines.len();
            let keep: Vec<MetricsLine> = lines
                .into_iter()
                .filter(|l| match l {
                    MetricsLine::Run { ts, .. } => {
                        let d = ts.date_naive();
                        !(d < eligible_before && self.rollup_path(agent, d).exists())
                    }
                    _ => true,
                })
                .collect();
            self.rewrite_agent_file(agent, &keep, keep.len() < n);
        }
        written
    }

    /// 4b: atomically rewrite one agent's raw file (temp file + rename —
    /// the same guarantee as `write_file`, so a crash never truncates the
    /// log). Skipped entirely when `rewrite` is false (nothing pruned).
    fn rewrite_agent_file(&self, agent: &str, lines: &[MetricsLine], rewrite: bool) {
        if !rewrite {
            return;
        }
        let path = self.agent_path(agent);
        let tmp = path.with_extension("jsonl.tmp");
        let mut content = String::new();
        for line in lines {
            if let Ok(json) = serde_json::to_string(line) {
                content.push_str(&json);
                content.push('\n');
            }
        }
        if let Err(e) = std::fs::write(&tmp, content) {
            tracing::warn!("4b: failed pruning metrics file {}: {e}", path.display());
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            tracing::warn!("4b: failed renaming pruned metrics file: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// 4b: the app-startup daily rollup — runs `roll_up_and_prune` over
    /// every agent at most once per UTC calendar day (marker file
    /// `metrics/.rollup-state`, plain date string). Returns the number of
    /// rollup files written (0 when today's rollup already ran).
    pub fn maybe_daily_rollup(&self, now: DateTime<Utc>, retention_days: u32) -> usize {
        let today = now.format("%Y-%m-%d").to_string();
        let marker = self.dir.join(".rollup-state");
        if let Ok(content) = std::fs::read_to_string(&marker) {
            if content.trim() == today {
                return 0;
            }
        }
        let mut count = 0;
        for agent in self.agent_names() {
            count += self.roll_up_and_prune(&agent, now, retention_days).len();
        }
        // Write the marker only after the loop so a crash mid-run retries
        // the rollup on the next startup (it is idempotent per file).
        if let Err(e) = std::fs::write(&marker, today) {
            tracing::debug!("4b: could not write rollup marker: {e}");
        }
        count
    }
    /// and the immediately preceding same-length window, both via the same
    /// windowed reader so the numbers match what the I5 effect check
    /// computes. Returns `(current, previous)`.
    pub fn compare(&self, agent: &str, days: u32) -> (MetricsReport, MetricsReport) {
        let now = Utc::now();
        let d = chrono::Duration::days(days as i64);
        let cur_start = now - d;
        (
            self.report(agent, Some(cur_start), Some(now)),
            self.report(agent, Some(cur_start - d), Some(cur_start)),
        )
    }

    /// 2d: gate-style comparison — the AFTER window `[since, now)` and the
    /// immediately preceding same-length window (length clamped to
    /// 1..=30 days, the evidence-gate rule), via the same windowed reader.
    /// Returns `(after, before)`.
    ///
    /// This is the single primitive the metric evidence gate
    /// (`MemoryManager::agent_metric_evidence`) AND the status rendering
    /// (`list_improvement_status`) share — so what the gate fired on and
    /// what the tool shows can never diverge.
    pub fn compare_since(&self, agent: &str, since: DateTime<Utc>) -> (MetricsReport, MetricsReport) {
        let now = Utc::now();
        let days = now.signed_duration_since(since).num_days().max(0);
        let window_days = i64::from(days).clamp(1, 30);
        let d = chrono::Duration::days(window_days);
        (
            self.report(agent, Some(since), Some(now)),
            self.report(agent, Some(since - d), Some(since)),
        )
    }

    /// All lines for `agent` with `ts >= since` (`None` = all time), oldest
    /// first — the raw-line twin of `summary_since` (the same window, no
    /// aggregation). The improvement-status views use it to show what
    /// actually happened since an agent's last check / last applied change.
    pub fn lines_since(&self, agent: &str, since: Option<DateTime<Utc>>) -> Vec<MetricsLine> {
        let mut lines: Vec<MetricsLine> = match since {
            Some(s) => self
                .read_all(agent)
                .into_iter()
                .filter(|l| l.ts() >= s)
                .collect(),
            None => self.read_all(agent),
        };
        // `read_all` is newest-first (display order); the documented
        // oldest-first order is re-sorted here — the status views render
        // chronological histories.
        lines.sort_by_key(|l| l.ts());
        lines
    }

    /// Aggregate counts for `agent` over lines with `start <= ts < end`
    /// (a `None` bound is unbounded; `start >= end` yields the empty
    /// summary). The single code path behind `summary_since` (end = `None`)
    /// and the I5 effect-check before/after windows. 4b: the run-derived
    /// rollups of pruned days inside the window are merged in (additive),
    /// so the I5 before/after windows stay correct across retention.
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
        let lines = self.read_all(agent);
        for line in &lines {
            let ts = line.ts();
            if let Some(start) = start {
                if ts < start {
                    continue;
                }
            }
            if let Some(end) = end {
                if ts >= end {
                    continue;
                }
            }
            accumulate(&mut s, line);
        }
        s += self.load_rollups_in(agent, start, end, &lines).summary;
        s
    }

    /// Skill names read in the cross-agent `skills.jsonl` log with
    /// `ts >= since` (`None` = all time), oldest first, deduplicated
    /// (first-seen order kept). Empty when no usage is recorded.
    pub fn skill_usage_since(&self, since: Option<DateTime<Utc>>) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for line in self.read_all(SKILLS_FILE_STEM) {
            if let MetricsLine::SkillUse { ts, skill, .. } = &line {
                if let Some(since) = since {
                    if *ts < since {
                        continue;
                    }
                }
                if !seen.iter().any(|n| n == skill) {
                    seen.push(skill.clone());
                }
            }
        }
        seen
    }

    /// 1c: the self-improvement loop's own cost over `ts >= since`:
    /// `(checks, tokens_in, tokens_out, suggestions)`. `agent` names one
    /// profile's file; `None` = fleet-wide (every agent file + the reserved
    /// fleet file).
    pub fn loop_cost_since(
        &self,
        agent: Option<&str>,
        since: DateTime<Utc>,
    ) -> (u32, u64, u64, u32) {
        let names: Vec<String> = match agent {
            Some(a) => vec![a.to_string()],
            None => {
                let mut n = self.agent_names();
                n.push(FLEET_FILE_STEM.to_string());
                n
            }
        };
        let mut checks = 0u32;
        let mut tokens_in = 0u64;
        let mut tokens_out = 0u64;
        let mut suggestions = 0u32;
        for name in names {
            let s = self.summary_since(&name, Some(since));
            checks += s.checks;
            tokens_in += s.check_tokens_in;
            tokens_out += s.check_tokens_out;
            suggestions += s.check_suggestions;
        }
        (checks, tokens_in, tokens_out, suggestions)
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
    stats: &RunStats,
    duration_ms: u64,
    outcome: RunOutcome,
    tokens_in: u64,
    tokens_out: u64,
    run_id: &str,
    session_id: &str,
) {
    MetricsLog::default().log_run(
        agent,
        stats,
        duration_ms,
        outcome,
        tokens_in,
        tokens_out,
        run_id,
        session_id,
    );
}

/// Record a context trim in the DEFAULT metrics log (writer hook for the
/// agent loop). Best-effort — the rot signal must never break the run.
pub fn record_trim(
    agent: &str,
    chars_before: u64,
    chars_after: u64,
    messages_removed: u32,
    brief_updated: bool,
    overflow: bool,
    run_id: &str,
) {
    MetricsLog::default().log_trim(
        agent,
        chars_before,
        chars_after,
        messages_removed,
        brief_updated,
        overflow,
        run_id,
    );
}

/// Record user feedback in the DEFAULT metrics log (writer hook for the chat
/// feedback path). Best-effort — independent of the memory store.
pub fn record_feedback(agent: &str, up: bool) {
    MetricsLog::default().log_feedback(agent, up);
}

/// Record a skill read in the DEFAULT metrics log (writer hook for the
/// read_skill tool). Best-effort — usage is a signal, never load-bearing.
pub fn record_skill_use(skill: &str) {
    MetricsLog::default().log_skill_use(skill);
}

/// Record a self-improvement check (the loop's own cost) in the DEFAULT
/// metrics log (writer hook for the improver). `scope` is "agent" (written
/// to `agent`'s file) or "fleet" (written to the reserved `fleet.jsonl`).
/// Best-effort — the cost line must never fail the check.
pub fn record_check(
    agent: &str,
    scope: &str,
    tokens_in: u64,
    tokens_out: u64,
    suggestions: usize,
    duration_ms: u64,
) {
    MetricsLog::default().log_check(
        agent,
        scope,
        tokens_in,
        tokens_out,
        suggestions,
        duration_ms,
    );
}
// ── 3a: fleet loop status (core shared by the read_metrics status=true
// tool and the egui fleet dashboard) ────────────────────────────────────

impl MetricsLog {
    /// 3a: the timestamp of the agent's most recent metric line of ANY
    /// kind (`None` = the agent has no lines yet) — the dashboard's
    /// "last activity" KPI.
    pub fn last_activity(&self, agent: &str) -> Option<DateTime<Utc>> {
        self.read_all(agent)
            .into_iter()
            .rev()
            .map(|line| match line {
                MetricsLine::Run { ts, .. }
                | MetricsLine::Feedback { ts, .. }
                | MetricsLine::SkillUse { ts, .. }
                | MetricsLine::Trim { ts, .. }
                | MetricsLine::Check { ts, .. }
                | MetricsLine::Eval { ts, .. } => ts,
            })
            .next()
    }
}
