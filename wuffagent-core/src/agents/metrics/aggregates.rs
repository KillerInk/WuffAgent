//! 2c/3b/4b: pure aggregation over metrics lines — `MetricsSummary` (and its
//! per-line `accumulate` fold), the report structs + `build_report` (with the
//! 4b rollup top-up), the 4a `RunDetail` join view, and the bucket summaries.
//! No file I/O here; the store lives in `log`.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::stats::bucket::{bucket_index_utc, bucket_starts, Granularity};

use super::schema::{FeedbackKind, MetricsLine, RunOutcome, percentile};

/// 4b: the merged totals of the agent's rollup files that fall inside the
/// report window AND before the raw file's oldest run line (see
/// `MetricsLog::load_rollups_in`). Private: only `build_report` consumes it.
#[derive(Debug, Default)]
pub(crate) struct RollupTotals {
    pub(crate) summary: MetricsSummary,
    pub(crate) cost_usd: f64,
    pub(crate) tools: std::collections::BTreeMap<String, (u32, u32, u64)>,
    pub(crate) models: std::collections::BTreeMap<String, u32>,
    pub(crate) trim_correlation: TrimCorrelation,
}

impl RollupTotals {
    pub(crate) fn add_rollup(&mut self, r: &MetricsRollup) {
        self.summary += r.summary;
        self.cost_usd += r.cost_usd;
        for (name, (calls, errors, total_ms)) in &r.tools {
            let e = self.tools.entry(name.clone()).or_insert((0, 0, 0));
            e.0 += calls;
            e.1 += errors;
            e.2 += total_ms;
        }
        for (model, runs) in &r.model_mix {
            *self.models.entry(model.clone()).or_insert(0) += runs;
        }
        self.trim_correlation.trimmed_runs += r.trim_correlation.trimmed_runs;
        self.trim_correlation.trimmed_gave_up += r.trim_correlation.trimmed_gave_up;
        self.trim_correlation.untrimmed_runs += r.trim_correlation.untrimmed_runs;
        self.trim_correlation.untrimmed_gave_up += r.trim_correlation.untrimmed_gave_up;
    }
}

/// 2c/3b: report over in-memory lines (the egui agent editor's cached vec) —
/// delegates to [`build_report`] with empty rollup totals (the editor never
/// reads rollup files; `MetricsLog::report` does).
pub fn metrics_report_from_lines(
    lines: &[MetricsLine],
    since: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> MetricsReport {
    build_report(lines, since, end, &RollupTotals::default())
}

/// 2c/3b + 4b: the full report computation over `lines`, topped up with the
/// rolled-up days `rt` (their run lines were pruned from the raw log, so
/// they are the ONLY source for those days). Percentiles stay raw-only: a
/// day's per-run distribution cannot be reconstructed from its aggregate.
pub(crate) fn build_report(
    lines: &[MetricsLine],
    since: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    rt: &RollupTotals,
) -> MetricsReport {
    let mut report = MetricsReport {
        summary: MetricsSummary::from_lines(lines),
        p50_duration_ms: None,
        p95_duration_ms: None,
        p50_tokens: None,
        p95_tokens: None,
        tool_stats: Vec::new(),
        model_mix: Vec::new(),
        cost_usd: rt.cost_usd,
        trim_correlation: rt.trim_correlation,
    };
    // The summary must reflect the SAME window as the per-run stats below:
    // re-filter for the window when one is active (from_lines is all-time).
    if since.is_some() || end.is_some() {
        report.summary = windowed_summary(lines, since, end);
    }
    // 4b: the rolled-up days of this window (additive; run-derived only).
    report.summary += rt.summary;

    let mut durations: Vec<f64> = Vec::new();
    let mut tokens: Vec<f64> = Vec::new();
    // 4b: pre-seeded with the rollups' per-tool / per-model raw sums.
    let mut tool_acc: std::collections::BTreeMap<String, (u32, u32, u64)> = rt.tools.clone();
    let mut models: std::collections::BTreeMap<String, u32> = rt.models.clone();
    // 4a: runs that experienced at least one trim, by the 1e run_id.
    let trimmed_ids: std::collections::HashSet<&str> = lines
        .iter()
        .filter_map(|l| match l {
            MetricsLine::Trim { run_id, .. } if !run_id.is_empty() => Some(run_id.as_str()),
            _ => None,
        })
        .collect();
    for line in lines {
        let MetricsLine::Run {
            ts,
            duration_ms,
            tokens_in,
            tokens_out,
            tools,
            model,
            cost_usd,
            ..
        } = line
        else {
            continue;
        };
        if let Some(s) = since {
            if *ts < s {
                continue;
            }
        }
        if let Some(e) = end {
            if *ts >= e {
                continue;
            }
        }
        durations.push(*duration_ms as f64);
        tokens.push((tokens_in + tokens_out) as f64);
        for t in tools {
            let e = tool_acc.entry(t.name.clone()).or_insert((0, 0, 0));
            e.0 += t.calls;
            e.1 += t.errors;
            e.2 += t.duration_ms;
        }
        let key = if model.is_empty() {
            "(unknown)"
        } else {
            model.as_str()
        };
        *models.entry(key.to_string()).or_insert(0) += 1;
        report.cost_usd += cost_usd;
        // 4a: trim correlation (the Run pattern binds the remaining fields
        // via `..`; destructure the two we need explicitly).
        if let MetricsLine::Run {
            run_id,
            outcome,
            ..
        } = line
        {
            let gave_up = matches!(outcome, RunOutcome::GaveUp);
            if trimmed_ids.contains(run_id.as_str()) {
                report.trim_correlation.trimmed_runs += 1;
                if gave_up {
                    report.trim_correlation.trimmed_gave_up += 1;
                }
            } else {
                report.trim_correlation.untrimmed_runs += 1;
                if gave_up {
                    report.trim_correlation.untrimmed_gave_up += 1;
                }
            }
        }
    }

    durations.sort_by(f64::total_cmp);
    tokens.sort_by(f64::total_cmp);
    report.p50_duration_ms = percentile(&durations, 50.0).map(|v| v.round() as u64);
    report.p95_duration_ms = percentile(&durations, 95.0).map(|v| v.round() as u64);
    report.p50_tokens = percentile(&tokens, 50.0).map(|v| v.round() as u64);
    report.p95_tokens = percentile(&tokens, 95.0).map(|v| v.round() as u64);
    report.tool_stats = tool_acc
        .into_iter()
        .map(|(name, (calls, errors, total_ms))| ReportToolStat {
            name,
            calls,
            errors,
            avg_ms: if calls > 0 { total_ms / calls as u64 } else { 0 },
        })
        .collect();
    report.model_mix = models.into_iter().collect();
    report
        .model_mix
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    report
}

/// 4a: the fully-joined view of one run — its own metrics line (with the
/// per-tool breakdown), every LLM round it made (usage.jsonl entries
/// carrying the same 1e `run_id`, in log order), and its run-level feedback
/// (3d). On-demand only: the caller reads both files once per call.
#[derive(Debug, Clone, PartialEq)]
pub struct RunDetail {
    /// The run's own metrics line (always the `Run` variant).
    pub run: MetricsLine,
    /// The run's LLM rounds (empty for pre-1e runs or a missing usage log).
    pub rounds: Vec<crate::usage::recorder::UsageEntry>,
    /// Run-level feedback lines for this run (3d; 0 or 1 in practice).
    pub feedback: Vec<MetricsLine>,
}

impl RunDetail {
    /// Sum of `total_tokens` across the run's rounds (0 when unlinked).
    pub fn total_round_tokens(&self) -> u64 {
        self.rounds.iter().map(|r| r.total_tokens as u64).sum()
    }
}

/// 3b: `MetricsLog::bucket_summary`'s bucketing as a pure fn over an already
/// loaded `Vec<MetricsLine>` (the egui 2a cache feeds it the cached vec —
/// no JSONL re-scan per frame). Same window/bucket math as the method.
pub fn bucket_summary_from_lines(
    lines: &[MetricsLine],
    granularity: Granularity,
    now: DateTime<Utc>,
) -> Vec<BucketSummary> {
    let mut buckets: Vec<BucketSummary> = bucket_starts(granularity, now)
        .into_iter()
        .map(|start| BucketSummary {
            start,
            runs: 0,
            tool_calls: 0,
            tool_errors: 0,
            gave_up: 0,
            verified_after_retry: 0,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms_sum: 0,
        })
        .collect();
    for line in lines {
        let MetricsLine::Run {
            ts,
            tool_calls,
            tool_errors,
            duration_ms,
            outcome,
            tokens_in,
            tokens_out,
            ..
        } = line
        else {
            continue;
        };
        let Some(i) = bucket_index_utc(granularity, now, *ts) else {
            continue;
        };
        let b = &mut buckets[i];
        b.runs += 1;
        b.tool_calls += tool_calls;
        b.tool_errors += tool_errors;
        b.tokens_in += tokens_in;
        b.tokens_out += tokens_out;
        b.duration_ms_sum += duration_ms;
        match outcome {
            RunOutcome::GaveUp => b.gave_up += 1,
            RunOutcome::VerifiedAfterRetry => b.verified_after_retry += 1,
            RunOutcome::Verified | RunOutcome::None => {}
        }
    }
    buckets
}

/// 3b: `MetricsSummary` over the window `ts >= since && ts < end` (an
/// all-time `from_lines` re-filtered by the same per-line computation) —
/// the windowed twin of `MetricsSummary::from_lines`.
fn windowed_summary(
    lines: &[MetricsLine],
    since: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> MetricsSummary {
    let mut s = MetricsSummary::default();
    for line in lines {
        let ts = line.ts();
        if let Some(s_) = since {
            if ts < s_ {
                continue;
            }
        }
        if let Some(e) = end {
            if ts >= e {
                continue;
            }
        }
        accumulate(&mut s, line);
    }
    s
}

/// 2c: one tool's aggregated breakdown over a report window (the
/// per-tool `ToolStat` folded across every run in the window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportToolStat {
    pub name: String,
    pub calls: u32,
    pub errors: u32,
    /// Mean duration per call (0 when `calls == 0`).
    pub avg_ms: u64,
}

/// 2c: a full metrics report over a window — everything `MetricsSummary`
/// aggregates, plus per-run distribution statistics (percentiles) and
/// the per-tool / per-model breakdowns that the summary folds away.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsReport {
    /// The window's aggregate counts (same computation as
    /// `summary_between` over the same window).
    pub summary: MetricsSummary,
    /// p50/p95 of `duration_ms` over the window's runs (None = no runs).
    pub p50_duration_ms: Option<u64>,
    pub p95_duration_ms: Option<u64>,
    /// p50/p95 of per-run total tokens (`tokens_in + tokens_out`).
    pub p50_tokens: Option<u64>,
    pub p95_tokens: Option<u64>,
    /// Per-tool breakdown (name-sorted; from each run's `tools` histogram).
    pub tool_stats: Vec<ReportToolStat>,
    /// (model, runs) per model used in the window, most-used first
    /// (ties: name-ascending; empty model reported as "(unknown)").
    pub model_mix: Vec<(String, u32)>,
    /// 2d: sum of the runs' estimated `cost_usd` (0.0 when unpriced).
    pub cost_usd: f64,
    /// 4a: correlation between context trims and gave-ups, joined by the
    /// 1e run_id.
    pub trim_correlation: TrimCorrelation,
}

/// 4b: one UTC day of an agent's `run` lines, rolled up when the raw lines
/// rotate out of the JSONL log (retention). The rollup covers ONLY run
/// lines: feedback/skill_use/trim/check/eval lines are never pruned, so the
/// run-unrelated `MetricsSummary` fields stay 0 and double-counting is
/// impossible when `report`/`summary_between` merge rollups with raw lines.
/// Every field is additive, so days merge into a report by simple sums.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsRollup {
    /// The UTC day this rollup covers (YYYY-MM-DD).
    pub day: String,
    /// The agent this rollup belongs to (mirrors the file stem).
    pub agent: String,
    /// The day's run-derived aggregate (run-unrelated fields are 0 — see
    /// the type docs).
    pub summary: MetricsSummary,
    /// Sum of the day's runs' `cost_usd` (0.0 when all unpriced).
    #[serde(default)]
    pub cost_usd: f64,
    /// Per-tool raw totals: name → (calls, errors, total_ms) — additive
    /// (the report turns this into `ReportToolStat` with the average).
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, (u32, u32, u64)>,
    /// Per-model run counts ("" → "(unknown)", the report's convention).
    #[serde(default)]
    pub model_mix: std::collections::BTreeMap<String, u32>,
    /// The day's trim correlation (run-based; all fields additive).
    #[serde(default)]
    pub trim_correlation: TrimCorrelation,
}

/// 4a: correlation — how often runs that experienced a context trim
/// (joined by the 1e `run_id`) ended in `gave_up`, versus runs that were
/// never trimmed. Pre-1e runs carry an empty run_id and can never link to a
/// trim line, so they count as untrimmed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimCorrelation {
    /// Runs with at least one trim line carrying the same run_id.
    pub trimmed_runs: u32,
    /// Of those, runs whose terminal outcome was `gave_up`.
    pub trimmed_gave_up: u32,
    /// Runs with no linked trim line (incl. pre-1e runs).
    pub untrimmed_runs: u32,
    /// Of those, runs whose terminal outcome was `gave_up`.
    pub untrimmed_gave_up: u32,
}

/// 2b: one zero-filled bucket of the metrics store's time window — the
/// metrics twin of the usage store's `Bucket` (same shared bucket math).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketSummary {
    /// Local wall-clock start of the bucket (oldest bucket first in the
    /// returned window).
    pub start: NaiveDateTime,
    /// Completed runs in this bucket.
    pub runs: u32,
    /// 3a: tool calls in this bucket (the denominator for the bucket's
    /// error rate).
    pub tool_calls: u32,
    /// Tool-call errors in this bucket.
    pub tool_errors: u32,
    /// Runs whose terminal outcome was `gave_up`.
    pub gave_up: u32,
    /// Runs verified only after a nudge retry.
    pub verified_after_retry: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Sum of `duration_ms` over the bucket's runs.
    pub duration_ms_sum: u64,
}

impl BucketSummary {
    /// 4a: fraction of the bucket's runs that needed a nudge retry to reach
    /// verification (`verified_after_retry / runs`; 0.0 for an empty bucket).
    pub fn retry_rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            self.verified_after_retry as f64 / self.runs as f64
        }
    }
}

/// Aggregate counts over an agent's metric lines (all-time or since `since`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// 3d: feedback lines attached to a specific run (`run_id` set) — the
    /// run-level subset of `feedback_up` (message-level = up - run_up).
    pub feedback_run_up: u32,
    /// 3d: run-level subset of `feedback_down` (see `feedback_run_up`).
    pub feedback_run_down: u32,
    /// Total wall-clock duration of the counted runs, in milliseconds
    /// (2c: lets the `read_metrics` tool and the effect check report
    /// duration averages/deltas without re-reading the raw lines).
    pub total_duration_ms: u64,
    /// 4c: total prompt tokens consumed by the counted runs (cost evidence).
    pub tokens_in: u64,
    /// 4c: total completion tokens produced by the counted runs.
    pub tokens_out: u64,
    /// Context trims applied to the counted agent's runs (context-rot
    /// signal: how often the model was forced to drop its own history).
    pub trims: u32,
    /// 1c: self-improvement checks in the window (the loop's own cost).
    pub checks: u32,
    /// 1c: prompt tokens consumed by the counted checks (the loop's cost).
    pub check_tokens_in: u64,
    /// 1c: completion tokens produced by the counted checks.
    pub check_tokens_out: u64,
    /// 1c: total suggestions produced by the counted checks.
    pub check_suggestions: u32,
    /// 2b: evals run in the window (the eval harness's pass/fail records).
    pub evals: u32,
    /// 2b: evals that passed in the window.
    pub evals_passed: u32,
    /// 2b: prompt tokens consumed by the counted evals.
    pub eval_tokens_in: u64,
    /// 2b: completion tokens produced by the counted evals.
    pub eval_tokens_out: u64,
}

impl MetricsSummary {
    /// 2a: aggregate `lines` with no time window — the shared computation
    /// behind `summary_between` (which filters by window first) and the egui
    /// metrics cache (which feeds it the full cached vec).
    pub fn from_lines(lines: &[MetricsLine]) -> Self {
        let mut s = Self::default();
        for line in lines {
            accumulate(&mut s, line);
        }
        s
    }

    /// One-line rendering with a caller-supplied label (the I5 effect check
    /// labels its before/after windows). Returns an empty string when there
    /// is nothing to report (no runs and no feedback).
    pub fn format_labeled(&self, label: &str) -> String {
        if self.runs == 0
            && self.feedback_up == 0
            && self.feedback_down == 0
            && self.trims == 0
        {
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
             tokens: {} in / {} out, user feedback: {} up / {} down, context trims: {})",
            self.runs,
            self.tool_calls,
            self.tool_errors,
            error_rate,
            self.verified,
            self.verified_after_retry,
            self.gave_up,
            self.not_verified,
            self.tokens_in,
            self.tokens_out,
            self.feedback_up,
            self.feedback_down,
            self.trims,
        )
    }

    /// One-line rendering for the improver prompt ("Recent metrics: …").
    pub fn format_line(&self) -> String {
        self.format_labeled("Recent metrics")
    }

    /// Average run duration in seconds ("0.0s" when there are no runs;
    /// 2c — the `read_metrics` tool and the effect check).
    pub fn avg_duration_secs(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            self.total_duration_ms as f64 / self.runs as f64 / 1000.0
        }
    }
}

/// 4b: every field is an additive sum, so rollups merge into summaries
/// field-wise (and the windowed summary of raw lines can be topped up with
/// the rolled-up days of the same window).
impl std::ops::AddAssign for MetricsSummary {
    fn add_assign(&mut self, o: Self) {
        self.runs += o.runs;
        self.tool_calls += o.tool_calls;
        self.tool_errors += o.tool_errors;
        self.verified += o.verified;
        self.verified_after_retry += o.verified_after_retry;
        self.gave_up += o.gave_up;
        self.not_verified += o.not_verified;
        self.feedback_up += o.feedback_up;
        self.feedback_down += o.feedback_down;
        self.feedback_run_up += o.feedback_run_up;
        self.feedback_run_down += o.feedback_run_down;
        self.total_duration_ms += o.total_duration_ms;
        self.tokens_in += o.tokens_in;
        self.tokens_out += o.tokens_out;
        self.trims += o.trims;
        self.checks += o.checks;
        self.check_tokens_in += o.check_tokens_in;
        self.check_tokens_out += o.check_tokens_out;
        self.check_suggestions += o.check_suggestions;
        self.evals += o.evals;
        self.evals_passed += o.evals_passed;
        self.eval_tokens_in += o.eval_tokens_in;
        self.eval_tokens_out += o.eval_tokens_out;
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
/// 2a: fold one metric line into an aggregate summary — the shared
/// per-line computation behind `summary_between` (which applies the time
/// window first) and `MetricsSummary::from_lines` (no window).
pub(crate) fn accumulate(s: &mut MetricsSummary, line: &MetricsLine) {
    match line {
        MetricsLine::Run {
            tool_calls,
            tool_errors,
            duration_ms,
            outcome,
            tokens_in,
            tokens_out,
            ..
        } => {
            s.runs += 1;
            s.tool_calls += tool_calls;
            s.tool_errors += tool_errors;
            s.total_duration_ms += duration_ms;
            s.tokens_in += tokens_in;
            s.tokens_out += tokens_out;
            match outcome {
                RunOutcome::Verified => s.verified += 1,
                RunOutcome::VerifiedAfterRetry => s.verified_after_retry += 1,
                RunOutcome::GaveUp => s.gave_up += 1,
                RunOutcome::None => s.not_verified += 1,
            }
        }
        MetricsLine::Feedback {
            feedback, run_id, ..
        } => match feedback {
            FeedbackKind::Up => {
                s.feedback_up += 1;
                if run_id.is_some() {
                    s.feedback_run_up += 1;
                }
            }
            FeedbackKind::Down => {
                s.feedback_down += 1;
                if run_id.is_some() {
                    s.feedback_run_down += 1;
                }
            }
        },
        MetricsLine::Trim { .. } => {
            s.trims += 1;
        }
        MetricsLine::SkillUse { .. } => {
            // Not counted in the per-agent summary (the skills file
            // is fleet-wide).
        }
        MetricsLine::Check {
            tokens_in,
            tokens_out,
            suggestions,
            ..
        } => {
            s.checks += 1;
            s.check_tokens_in += tokens_in;
            s.check_tokens_out += tokens_out;
            s.check_suggestions += *suggestions as u32;
        }
        MetricsLine::Eval {
            passed,
            tokens_in,
            tokens_out,
            ..
        } => {
            s.evals += 1;
            if *passed {
                s.evals_passed += 1;
            }
            s.eval_tokens_in += tokens_in;
            s.eval_tokens_out += tokens_out;
        }
    }
}
