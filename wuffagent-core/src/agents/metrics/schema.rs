//! M1: the metrics line schema — `MetricsLine` and its terminal
//! outcome/feedback enums, plus the `percentile` helper the reports use.
//! (See the module docs in `mod.rs` for the line-kind catalogue.)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agents::types::ToolStat;

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
        /// 4d: schema version of this line's fields (0 = pre-4d line, 1 =
        /// current). A pure migration anchor: old lines without it parse as
        /// 0, writers stamp 1.
        #[serde(default)]
        v: u32,
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
        /// 4d: schema version (0 = pre-4d line, 1 = current).
        #[serde(default)]
        v: u32,
    },
    /// An agent read a skill (usage signal for the improver).
    SkillUse {
        /// UTC timestamp of the read.
        ts: DateTime<Utc>,
        /// Skill name (slug) that was read.
        skill: String,
        /// 4d: schema version (0 = pre-4d line, 1 = current).
        #[serde(default)]
        v: u32,
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
        /// 4d: schema version (0 = pre-4d line, 1 = current).
        #[serde(default)]
        v: u32,
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
        /// 4d: schema version (0 = pre-4d line, 1 = current).
        #[serde(default)]
        v: u32,
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
        /// 4d: schema version (0 = pre-4d line, 1 = current).
        #[serde(default)]
        v: u32,
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
                ..
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
            MetricsLine::Feedback { ts, feedback, run_id, .. } => format!(
                "{} feedback: {}{}",
                ts.format("%Y-%m-%d %H:%M"),
                if *feedback == FeedbackKind::Up { "up" } else { "down" },
                run_id
                    .as_deref()
                    .map(|id| format!(" (run {id})"))
                    .unwrap_or_default()
            ),
            MetricsLine::SkillUse { ts, skill, .. } => {
                format!("{} skill used: {}", ts.format("%Y-%m-%d %H:%M"), skill)
            }
            MetricsLine::Trim {
                ts,
                chars_before,
                chars_after,
                messages_removed,
                brief_updated,
                overflow,
                ..
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
                ..
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
                ..
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
