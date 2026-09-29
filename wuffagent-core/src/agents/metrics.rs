//! M1: per-agent run metrics — append-only JSONL, one file per agent.
//!
//! Each line lives in `<wuffagent_home>/metrics/<agent>.jsonl`:
//! ```json
//! {"kind":"run","ts":"2026-09-25T13:44:15.123Z","tool_calls":12,"tool_errors":2,"verification_attempts":1,"duration_ms":45210,"outcome":"verified"}
//! {"kind":"feedback","ts":"2026-09-25T13:50:00.000Z","feedback":"up"}
//! ```
//!
//! Line kinds, one file per agent (the file name IS the agent name, so the
//! line carries no agent field):
//! - `run` — one completed agent LLM-loop (written at `run_llm_loop` end;
//!   each handoff hop records its own line under its own agent).
//! - `feedback` — a 👍/👎 the user gave on an assistant answer
//!   (written by the chat feedback path, independent of the memory store).
//! - `skill_use` — an agent `read_skill`-ed a skill (written by the
//!   read_skill tool). Skills are shared across agents (no per-agent
//!   attribution at the tool layer), so they go to the reserved
//!   `skills.jsonl` file — `agent_names()` skips it for the fleet summary.
//! - `trim` — the agent's context was trimmed before an LLM call (written
//!   by the agent loop at both trim sites: the proactive 90%→50% trim and
//!   the 400-exceed-context backstop). The context-rot signal: how often
//!   the model is forced to drop its own history, and whether the mission
//!   brief re-anchored the state.
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

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agents::types::{RunStats, ToolStat};
use crate::stats::bucket::{bucket_index_utc, bucket_starts, Granularity};

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
        /// 4c: prompt tokens consumed across the run's LLM rounds (summed
        /// from server-reported usage; 0 when the server reports none).
        #[serde(default)]
        tokens_in: u64,
        /// 4c: completion tokens produced across the run's LLM rounds.
        #[serde(default)]
        tokens_out: u64,
        /// 1a: per-tool breakdown (folded; empty for lines written before
        /// 1a, so old stores parse unchanged).
        #[serde(default)]
        tools: Vec<ToolStat>,
        /// 1e: per-run join key — correlates this Run line with the run's
        /// Trim lines and the usage.jsonl UsageEntry lines. Empty on lines
        /// written before 1e.
        #[serde(default)]
        run_id: String,
        /// 1e: the session the run belonged to (empty on pre-1e lines).
        #[serde(default)]
        session_id: String,
        /// 1d: wall-clock ms spent in the LLM (main rounds + verification
        /// judge). Empty/0 on lines written before 1d.
        #[serde(default)]
        llm_ms: u64,
        /// 1d: wall-clock ms spent in tool execution (the per-tool
        /// histogram's sum). 0 on lines written before 1d. Always
        /// `llm_ms + tools_ms <= duration_ms`.
        #[serde(default)]
        tools_ms: u64,
        /// 1b: model name of the run's LLM calls ("" = unknown/unreported).
        #[serde(default)]
        model: String,
        /// 1b: estimated cost in USD (0.0 = recorded but unpriced).
        #[serde(default)]
        cost_usd: f64,
    },
    /// User feedback on an assistant answer.
    Feedback {
        /// UTC timestamp of the rating.
        ts: DateTime<Utc>,
        feedback: FeedbackKind,
        /// 3d: the specific run the rating is about (`Some(run_id)` =
        /// run-level feedback, `None` = legacy message-level rating).
        #[serde(default)]
        run_id: Option<String>,
    },
    /// An agent read a skill (usage signal for the improver).
    SkillUse {
        /// UTC timestamp of the read.
        ts: DateTime<Utc>,
        /// Skill name (slug) that was read.
        skill: String,
    },
    /// An agent's context was trimmed before an LLM call (context-rot
    /// signal — how often the model loses its own history mid-run).
    Trim {
        /// UTC timestamp of the trim.
        ts: DateTime<Utc>,
        /// Estimated chars of the request before the trim (messages only,
        /// the same metric as `chars_after`, so the delta is meaningful).
        chars_before: u64,
        /// Estimated chars of the request after the trim.
        chars_after: u64,
        /// Messages removed by the trim.
        messages_removed: u32,
        /// A mission brief was present in the post-trim context (the state
        /// re-anchoring — task/corrections/decisions/files — was available to
        /// the model after the drop).
        brief_updated: bool,
        /// 1e: the run this trim happened in (empty on pre-1e lines).
        #[serde(default)]
        run_id: String,
        /// The trim was the 400-exceed-context backstop (force-trim to 85%
        /// of the reported window) rather than the proactive 90%→50% trim.
        /// Frequent `true` means the estimator/trigger is miscalibrated.
        overflow: bool,
    },
    /// 1c: one self-improvement check (the loop's OWN cost — an improver LLM
    /// call). The line is stored in the reviewed profile's file (scope
    /// "agent") or the reserved `fleet.jsonl` (scope "fleet"), so `agent`
    /// mirrors the file stem and keeps `describe()` self-contained.
    Check {
        /// UTC timestamp of the check's end.
        ts: DateTime<Utc>,
        /// Profile reviewed ("fleet" for a fleet-wide check) — mirrors the
        /// file the line is stored in.
        agent: String,
        /// Check scope: "agent" (single-profile review) or "fleet".
        scope: String,
        /// Prompt tokens consumed by the check's LLM call (0 when the server
        /// reports none).
        #[serde(default)]
        tokens_in: u64,
        /// Completion tokens produced by the check's LLM call (0 when the
        /// server reports none).
        #[serde(default)]
        tokens_out: u64,
        /// Suggestions the check produced (0 = nothing to improve).
        #[serde(default)]
        suggestions: usize,
        /// Wall-clock duration of the check's LLM call, in milliseconds.
        #[serde(default)]
        duration_ms: u64,
    },
    /// 2b: a golden/regression eval run (the eval harness's pass/fail record,
    /// written by `run_eval`). `id` is the eval's id (per-eval tracking);
    /// `passed` is the verification judge's verdict against the eval's
    /// `expect`; `score` is an optional 0.0-1.0 quality score (2d).
    Eval {
        ts: DateTime<Utc>,
        /// Profile that ran the eval — mirrors the file the line is stored in.
        #[serde(default)]
        agent: String,
        /// The eval's id (per-eval tracking; empty for ad-hoc runs).
        #[serde(default)]
        id: String,
        /// Whether the verification judge passed the response against the
        /// eval's `expect`.
        #[serde(default)]
        passed: bool,
        /// Optional 0.0-1.0 quality score (2d: LLM-graded; `None` = pass/fail only).
        #[serde(default)]
        score: Option<f64>,
        /// Wall-clock duration of the eval run, in milliseconds.
        #[serde(default)]
        duration_ms: u64,
        /// Prompt tokens consumed by the eval run (0 when the server reports none).
        #[serde(default)]
        tokens_in: u64,
        /// Completion tokens produced by the eval run (0 when the server reports none).
        #[serde(default)]
        tokens_out: u64,
        /// 1b: model name the eval ran on ("" = server reported none).
        #[serde(default)]
        model: String,
        /// 1b: estimated cost in USD (0.0 = recorded but unpriced).
        #[serde(default)]
        cost_usd: f64,
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
                tokens_in,
                tokens_out,
                tools,
                run_id: _,
                session_id: _,
                llm_ms: _,
                tools_ms: _,
                model: _,
                cost_usd: _,
            } => {
                let mut s = format!(
                    "{} run: {} tool calls ({} errors), {} verification attempt(s), {:.1}s, {} tokens in / {} out, outcome: {}",
                    ts.format("%Y-%m-%d %H:%M"),
                    tool_calls,
                    tool_errors,
                    verification_attempts,
                    *duration_ms as f64 / 1000.0,
                    tokens_in,
                    tokens_out,
                    outcome.as_str(),
                );
                // 1a: per-tool breakdown (top 5 by duration, compact form
                // name=calls/errors/ms) — the improver's slow/error-prone
                // tool evidence, right on the run line.
                if !tools.is_empty() {
                    let mut sorted = tools.clone();
                    sorted.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms));
                    let seg: Vec<String> = sorted
                        .iter()
                        .take(5)
                        .map(|t| format!(
                            "{}={}/e{}/{}ms",
                            t.name, t.calls, t.errors, t.duration_ms
                        ))
                        .collect();
                    s.push_str("; tools: ");
                    s.push_str(&seg.join(", "));
                }
                s
            }
            MetricsLine::Feedback { ts, feedback, run_id } => format!(
                "{} feedback: {}{}",
                ts.format("%Y-%m-%d %H:%M"),
                if *feedback == FeedbackKind::Up { "up" } else { "down" },
                run_id
                    .as_deref()
                    .map(|id| format!(" (run {id})"))
                    .unwrap_or_default()
            ),
            MetricsLine::SkillUse { ts, skill } => {
                format!("{} skill used: {}", ts.format("%Y-%m-%d %H:%M"), skill)
            }
            MetricsLine::Trim {
                ts,
                chars_before,
                chars_after,
                messages_removed,
                brief_updated,
                overflow,
                run_id: _,
            } => format!(
                "{} context trim{}: {} messages removed ({} → {} chars){}",
                ts.format("%Y-%m-%d %H:%M"),
                if *overflow { " (overflow backstop)" } else { "" },
                messages_removed,
                chars_before,
                chars_after,
                if *brief_updated { ", brief updated" } else { "" },
            ),
            MetricsLine::Check {
                ts,
                agent,
                scope,
                tokens_in,
                tokens_out,
                suggestions,
                duration_ms,
            } => format!(
                "{} improvement check ({scope}, {agent}): {tokens_in} tok in / {tokens_out} out, \
                 {suggestions} suggestion(s), {:.1}s",
                ts.format("%Y-%m-%d %H:%M"),
                *duration_ms as f64 / 1000.0,
            ),
            MetricsLine::Eval {
                ts,
                agent,
                id,
                passed,
                score,
                duration_ms,
                tokens_in,
                tokens_out,
                model: _,
                cost_usd: _,
            } => format!(
                "{} eval ({agent}, id={id}): {}{} {} tok in / {} out, {:.1}s",
                ts.format("%Y-%m-%d %H:%M"),
                if *passed { "PASS" } else { "FAIL" },
                score.map(|s| format!(" (score {:.2})", s)).unwrap_or_default(),
                tokens_in,
                tokens_out,
                *duration_ms as f64 / 1000.0,
            ),
        }
    }

    /// 3b: the line's UTC timestamp (every variant carries one) — lets the
    /// egui cache (which holds `Vec<MetricsLine>`) window lines without the
    /// field being public.
    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            MetricsLine::Run { ts, .. }
            | MetricsLine::Feedback { ts, .. }
            | MetricsLine::SkillUse { ts, .. }
            | MetricsLine::Trim { ts, .. }
            | MetricsLine::Check { ts, .. }
            | MetricsLine::Eval { ts, .. } => *ts,
        }
    }
}

/// 2c: percentile of an ASCENDING-sorted slice, `p` on a 0..=100 scale,
/// linear interpolation between the two nearest ranks (numpy-default
/// semantics). `None` for an empty slice or a non-finite `p`; a single
/// element returns itself for every `p`. Out-of-range `p` is clamped.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() || !p.is_finite() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let clamped = p.clamp(0.0, 100.0);
    let rank = clamped / 100.0 * (sorted.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = rank - lo as f64;
    Some(sorted[lo] + frac * (sorted[hi] - sorted[lo]))
}

/// 3b: `MetricsReport::report`'s computation as a pure fn over an already
/// loaded `Vec<MetricsLine>` — the egui agent editor (2a cache) feeds it the
/// cached vec per selected window instead of re-scanning the JSONL per frame.
/// Window semantics identical to `report`: `ts >= since && ts < end`
/// (`None` bounds = unbounded).
pub fn metrics_report_from_lines(
    lines: &[MetricsLine],
    since: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> MetricsReport {
    let mut report = MetricsReport {
        summary: MetricsSummary::from_lines(lines),
        p50_duration_ms: None,
        p95_duration_ms: None,
        p50_tokens: None,
        p95_tokens: None,
        tool_stats: Vec::new(),
        model_mix: Vec::new(),
        cost_usd: 0.0,
        trim_correlation: Default::default(),
    };
    // The summary must reflect the SAME window as the per-run stats below:
    // re-filter for the window when one is active (from_lines is all-time).
    if since.is_some() || end.is_some() {
        report.summary = windowed_summary(lines, since, end);
    }

    let mut durations: Vec<f64> = Vec::new();
    let mut tokens: Vec<f64> = Vec::new();
    let mut tool_acc: std::collections::BTreeMap<String, (u32, u32, u64)> =
        std::collections::BTreeMap::new();
    let mut models: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
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

/// 4a: correlation — how often runs that experienced a context trim
/// (joined by the 1e `run_id`) ended in `gave_up`, versus runs that were
/// never trimmed. Pre-1e runs carry an empty run_id and can never link to a
/// trim line, so they count as untrimmed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

/// The timestamp of a metrics line (all variants carry one).
fn line_ts(line: &MetricsLine) -> &DateTime<Utc> {
    match line {
        MetricsLine::Run { ts, .. }
        | MetricsLine::Feedback { ts, .. }
        | MetricsLine::SkillUse { ts, .. }
        | MetricsLine::Trim { ts, .. }
        | MetricsLine::Check { ts, .. }
        | MetricsLine::Eval { ts, .. } => ts,
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
    /// per-run percentiles and the tool/model breakdowns.
    pub fn report(
        &self,
        agent: &str,
        since: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> MetricsReport {
        // 3b: the computation lives in the pure `metrics_report_from_lines`
        // (shared with the egui agent editor's cached vec) — this method is
        // just the file read + delegate.
        metrics_report_from_lines(&self.read_all(agent), since, end)
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

    /// 2c: before/after comparison — the CURRENT window `[now-days, now)`
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
                .filter(|l| *line_ts(l) >= s)
                .collect(),
            None => self.read_all(agent),
        };
        // `read_all` is newest-first (display order); the documented
        // oldest-first order is re-sorted here — the status views render
        // chronological histories.
        lines.sort_by_key(|l| *line_ts(l));
        lines
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
            let ts = line_ts(&line);
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
            accumulate(&mut s, &line);
        }
        s
    }

    /// Skill names read in the cross-agent `skills.jsonl` log with
    /// `ts >= since` (`None` = all time), oldest first, deduplicated
    /// (first-seen order kept). Empty when no usage is recorded.
    pub fn skill_usage_since(&self, since: Option<DateTime<Utc>>) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for line in self.read_all(SKILLS_FILE_STEM) {
            if let MetricsLine::SkillUse { ts, skill } = &line {
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

mod reader;

pub use reader::MetricsLogReader;

/// 2a: fold one metric line into an aggregate summary — the shared
/// per-line computation behind `summary_between` (which applies the time
/// window first) and `MetricsSummary::from_lines` (no window).
fn accumulate(s: &mut MetricsSummary, line: &MetricsLine) {
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

/// 3a: the loop half of the fleet status — cost-control settings and the
/// global (legacy) baseline. `None` in [`FleetLoopStatus`] when no memory
/// manager was wired (standalone tools/tests degrade to metrics only).
#[derive(Debug, Clone, Default)]
pub struct LoopConfigInfo {
    pub auto_improve: bool,
    pub cooldown_tasks: usize,
    pub min_interval_hours: u32,
    pub lessons: usize,
    /// Global (legacy) last check; per-agent baselines live on the rows.
    pub last_check: Option<DateTime<Utc>>,
    /// Whether new Lesson evidence exists since the global last check.
    pub new_evidence: bool,
}

/// 3a: one agent row of the fleet status — its loop state (if ever
/// checked) joined with its windowed metrics and its most recent line
/// (`describe()` text — what actually happened in the window).
#[derive(Debug, Clone)]
pub struct FleetAgentStatus {
    pub name: String,
    pub loop_state: Option<crate::memory::types::AgentImprovementState>,
    /// Windowed metrics (`ts >= since`); all-zero when the agent had no
    /// lines in the window.
    pub summary: MetricsSummary,
    /// The window's newest line rendered for humans (None = none).
    pub last_line: Option<String>,
}

/// 3a: fleet-wide token spend over the window (from the metrics store —
/// the runs the loop improves, not the improvement checks' own calls).
#[derive(Debug, Clone, Copy, Default)]
pub struct FleetSpend {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub runs: u32,
}

/// 3a: structured fleet loop status — the single implementation behind
/// BOTH the `read_metrics status=true` tool (renders it as text) and the
/// egui fleet dashboard (draws it), so the two surfaces cannot diverge.
#[derive(Debug, Clone, Default)]
pub struct FleetLoopStatus {
    pub window_days: u64,
    pub loop_config: Option<LoopConfigInfo>,
    /// One row per agent in the UNION of the metrics store's agent files
    /// and the memory manager's per-agent loop states (name-sorted) —
    /// loop-only agents still appear (with all-zero window metrics).
    pub agents: Vec<FleetAgentStatus>,
    pub fleet_spend: FleetSpend,
}

/// 3a: build the structured fleet loop status over the last `days` days.
/// Read-only (file reads + in-memory state); the window is `[now-days, now)`.
pub fn fleet_loop_status(
    log: &MetricsLog,
    memory: Option<&crate::memory::MemoryManager>,
    days: u64,
) -> FleetLoopStatus {
    let st = memory.map(|m| m.improvement_status());
    let loop_config = st.as_ref().map(|st| LoopConfigInfo {
        auto_improve: st.auto_improve,
        cooldown_tasks: st.improvement_cooldown_tasks,
        min_interval_hours: st.improvement_min_interval_hours,
        lessons: st.lesson_count,
        last_check: st.last_check,
        new_evidence: st.has_new_evidence,
    });

    let mut names: std::collections::BTreeSet<String> =
        log.agent_names().into_iter().collect();
    if let Some(st) = &st {
        for name in st.agents.keys() {
            names.insert(name.clone());
        }
    }

    let since = Utc::now() - chrono::Duration::days(days as i64);
    let mut agents = Vec::new();
    let mut spend = FleetSpend::default();
    for name in names {
        let summary = log.summary_between(&name, Some(since), None);
        spend.tokens_in += summary.tokens_in;
        spend.tokens_out += summary.tokens_out;
        spend.runs += summary.runs;
        let last_line = log.lines_since(&name, Some(since)).pop().map(|l| l.describe());
        let loop_state = st.as_ref().and_then(|s| s.agents.get(&name).cloned());
        agents.push(FleetAgentStatus {
            name,
            loop_state,
            summary,
            last_line,
        });
    }
    FleetLoopStatus {
        window_days: days,
        loop_config,
        agents,
        fleet_spend: spend,
    }
}

#[cfg(test)]
mod tests;
