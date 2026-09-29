//! Improvement-loop policy: the no-op backoff multiplier, the
//! deterministic before/after effect verdict, and the "is the effect check
//! awaiting enough samples" gate (the loop's "should we re-check now" and
//! "did the last change help" rules).

use crate::memory::manager::MemoryManager;
use crate::memory::types::MemoryEntry;
/// 2a: the no-op-streak backoff multiplier for the per-agent improvement
/// cooldown (used by `MemoryManager::agent_improvement_due`): streak 1: x1,
/// 2: x2, 3: x3, 4+: x4. A check that produces nothing slows the re-check
/// cadence down (bounded, so the agent is never starved forever); a
/// productive check resets the streak to 0.
pub fn no_op_backoff_multiplier(no_op_streak: u32) -> u64 {
    1 + no_op_streak.saturating_sub(1).min(3) as u64
}

/// 2a: deterministic effect verdict for the I5 before/after comparison.
///
/// Heuristic, intentionally simple and explainable: it looks at the tool
/// ERROR RATE delta (±1 percentage point threshold) over the two equal
/// windows; too few after-runs (or none at all) is "inconclusive". Duration
/// and outcomes stay in the prompt text for the LLM's qualitative half.
///
/// 2e: `min_samples` (config `improvement_min_samples`) is the after-run
/// floor before a verdict is issued.
pub fn effect_verdict(
    before: &crate::agents::metrics::MetricsSummary,
    after: &crate::agents::metrics::MetricsSummary,
    min_samples: u32,
) -> &'static str {
    if after.runs == 0 {
        return "inconclusive (no runs after the change)";
    }
    if after.runs < min_samples.max(1) {
        return "inconclusive (low sample after the change)";
    }
    let err_rate = |s: &crate::agents::metrics::MetricsSummary| {
        if s.tool_calls == 0 {
            0.0
        } else {
            s.tool_errors as f64 / s.tool_calls as f64
        }
    };
    let delta_pp = (err_rate(after) - err_rate(before)) * 100.0;
    if delta_pp <= -1.0 {
        "improved"
    } else if delta_pp >= 1.0 {
        "regressed"
    } else {
        "neutral"
    }
}
/// I5: the most recent "prompt changed via approved improvement" marker for
/// this agent (recorded by the review panel when the user approves a prompt
/// change; a Fact entry tagged `improvement-applied` + `agent:<name>`).
pub(crate) fn latest_applied_marker(manager: &MemoryManager, agent_name: &str) -> Option<MemoryEntry> {
    manager
        .get_by_tag("improvement-applied")
        .into_iter()
        .filter(|m| m.tags.iter().any(|t| t == &format!("agent:{agent_name}")))
        .max_by_key(|m| m.timestamp)
}
/// 1b: when the effect check is below its sample floor, return `(have, need)`
/// so the status view can show "awaiting samples" instead of reading the
/// (stale or `None`) `last_effect_verdict`. `None` means either no approved
/// change has been recorded for this agent (no effect check to await) or
/// enough runs have accumulated since it (the verdict path applies).
///
/// Takes the metrics log explicitly (rather than `MetricsLog::default()`) so
/// the status tool can pass the same log it uses for its metrics join.
pub fn effect_check_awaiting_samples(
    manager: &MemoryManager,
    agent_name: &str,
    metrics: &crate::agents::metrics::MetricsLog,
) -> Option<(u32, u32)> {
    let marker = latest_applied_marker(manager, agent_name)?;
    let since = marker.timestamp?;
    let after = metrics.summary_between(agent_name, Some(since), None);
    let need = manager.config().improvement_min_samples.max(1);
    (after.runs < need).then_some((after.runs, need))
}
