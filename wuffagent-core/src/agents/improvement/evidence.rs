//! Evidence collection + prompt-section formatting for the improver:
//! lesson gathering, rejection history, the applied-change marker, and the
//! I5 effect-check section (metrics/evals windows) the improver prompt
//! splices in.

use crate::agents::RunStats;
use crate::memory::manager::MemoryManager;
use crate::memory::types::MemoryEntry;

use super::policy::{effect_verdict, latest_applied_marker};
use super::{LESSON_CHAR_BUDGET, NEWLINE, truncate_for_evidence};
/// Gather relevant lesson memories for an improvement check.
///
/// Tag first (S3): lessons saved with the `agent:<name>` tag (the convention
/// the memory tool description and system-prompt hints ask for) are the
/// strongest per-agent signal. Fall back to the previous behavior — search by
/// agent name + task text, then the most recent lessons — when the tag search
/// finds nothing (backward compatible: old entries simply don't match the
/// tag).
pub(crate) fn collect_lessons(manager: &MemoryManager, agent_name: &str, task: &str) -> Vec<String> {
    let from_tag = manager.get_by_tag(&format!("agent:{agent_name}"));
    let mut lessons: Vec<String> = from_tag
        .iter()
        .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
        .map(|m| m.content.clone())
        .collect();
    if lessons.is_empty() {
        let from_search = manager.search(&format!("{} {}", agent_name, task));
        lessons = from_search
            .iter()
            .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
            .map(|m| m.content.clone())
            .collect();
    }
    if lessons.is_empty() {
        lessons = manager
            .get_recent(10)
            .iter()
            .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
            .map(|m| m.content.clone())
            .collect();
    }
    lessons
}

/// I1: previously rejected improvement suggestions for this agent (F5
/// dismissal lessons, tagged `improvement-rejected` + `agent:<name>`).
pub(crate) fn rejected_history(manager: &MemoryManager, agent_name: &str) -> Vec<String> {
    manager
        .get_by_tag("improvement-rejected")
        .into_iter()
        .filter(|m| m.tags.iter().any(|t| t == &format!("agent:{agent_name}")))
        .map(|m| m.content)
        .collect()
}

/// I5: this agent's recorded outcomes since `since` — Lesson entries tagged
/// `agent:<name>` (S1 verification verdicts, S2 user feedback, S3 agent
/// lessons). Facts (e.g. the I5 marker itself) are not outcomes.
pub(crate) fn outcomes_since(
    manager: &MemoryManager,
    agent_name: &str,
    since: chrono::DateTime<chrono::Utc>,
) -> Vec<MemoryEntry> {
    manager
        .get_by_tag(&format!("agent:{agent_name}"))
        .into_iter()
        .filter(|m| m.r#type == crate::memory::MemoryType::Lesson)
        .filter(|m| m.timestamp.map(|t| t > since).unwrap_or(false))
        .collect()
}
/// I5: the "effect check" section of the improver prompt — how long ago the
/// last approved prompt change happened and which outcomes were recorded for
/// this agent since it, so the improver can propose a revert when the change
/// made things worse.
///
/// Returns `(prompt section, evidence line)`, or `None` when no approved
/// change has been recorded for this agent yet (the prompt gets no section).
pub(crate) fn effect_check_section(manager: &MemoryManager, agent_name: &str) -> Option<(String, String)> {
    let marker = latest_applied_marker(manager, agent_name)?;
    let since = marker.timestamp?;
    let days = chrono::Utc::now()
        .signed_duration_since(since)
        .num_days()
        .max(0);
    let date = since.format("%Y-%m-%d").to_string();

    // Most recent first, capped so the section can't blow the prompt budget.
    let mut outcomes = outcomes_since(manager, agent_name, since);
    outcomes.sort_by_key(|m| m.timestamp);
    let outcomes: Vec<&MemoryEntry> = outcomes.iter().rev().take(10).collect();

    let list = if outcomes.is_empty() {
        "(none yet)".to_string()
    } else {
        let mut list = String::new();
        for (i, m) in outcomes.iter().enumerate() {
            if i > 0 {
                list.push(NEWLINE);
            }
            list.push_str(&truncate_for_evidence(&m.content));
        }
        list
    };

    // 1a: the quantitative half of the effect check — per-agent metrics in
    // two windows: AFTER = change → now, BEFORE = the immediately preceding
    // period of the same length (min 1 day so a fresh change still gets a
    // baseline, max 30 days so old markers don't sweep in months of
    // history). The lesson list above stays as the qualitative half.
    let metrics_log = crate::agents::metrics::MetricsLog::default();
    let window_days = days.clamp(1, 30);
    let before_start = since - chrono::Duration::days(window_days);
    let before = metrics_log.summary_between(agent_name, Some(before_start), Some(since));
    let after = metrics_log.summary_between(agent_name, Some(since), None);
    // 2c: the evals half of the effect check (deterministic pass/fail in the
    // same windows) — computed once and folded into both return paths below.
    let evals_block = evals_window_lines(before_start, since, &before, &after);
    // 1b: sample floor — below this many runs after the change the metrics
    // cannot be judged. Defer instead of settling: emit a short section
    // (the before/after metrics block is omitted) and do NOT record a
    // verdict, so `last_effect_verdict` stops flapping to "inconclusive" on
    // every check and the status view can surface "awaiting samples
    // (have/need)" instead of a pseudo-result. The qualitative outcomes list
    // is still shown (it does not depend on the metrics sample).
    let min_samples = manager.config().improvement_min_samples.max(1);
    if after.runs < min_samples {
        let mut section = format!(
            "The prompt for '{agent_name}' was last changed via an approved improvement {days} day(s) ago ({date}), but only {have} run(s) have been recorded since (need {min_samples}) — not yet judgeable; do not propose a revert based on the metrics.\nOutcomes recorded for this agent since that change:\n{list}",
            have = after.runs
        );
        if let Some(eb) = &evals_block {
            section.push_str(&format!("\n{eb}"));
        }
        let evidence_line = format!(
            "Effect check: change applied {days} day(s) ago ({date}); awaiting samples ({have}/{min_samples}); no verdict recorded",
            have = after.runs
        );
        return Some((section, evidence_line));
    }

    // 1a: the quantitative half of the effect check (only reached with a
    // judgeable after-window): per-agent metrics in two windows — AFTER =
    // change → now, BEFORE = the immediately preceding period of the same
    // length (min 1 day so a fresh change still gets a baseline, max 30 days
    // so old markers don't sweep in months of history). The lesson list above
    // stays as the qualitative half.
    let before_line = window_metrics_line(
        &format!(
            "metrics before the change ({} → {})",
            before_start.format("%Y-%m-%d"),
            date,
        ),
        &before,
    );
    let after_line = window_metrics_line(
        &format!("metrics after the change ({date} → now)"),
        &after,
    );

    let mut section = format!(
        "The prompt for '{agent_name}' was last changed via an approved improvement {days} day(s) ago ({date}).\n{before_line}\n{after_line}\nOutcomes recorded for this agent since that change:\n{list}\nIf the after-change metrics or outcomes look worse than before, you may propose reverting the prompt (prompt_change set to the previous prompt text) — the user can always revert from history."
    );
    // 2c: append the deterministic evals window (if any) to the full section.
    if let Some(eb) = &evals_block {
        section.push_str(&format!("\n{eb}"));
    }
    // 2a: deterministic effect verdict, persisted for the status view and
    // appended to the evidence line (best-effort write; the LLM still gets
    // the raw windows above for its qualitative half).
    let verdict = effect_verdict(&before, &after, min_samples);
    manager.record_effect_verdict(agent_name, verdict);

    let evidence_line = format!(
        "Effect check: prompt last changed via approved improvement {days} day(s) ago ({date}); runs before vs after: {} vs {}; {} outcome(s) since; verdict: {verdict}",
        before.runs, after.runs, outcomes.len()
    );
    Some((section, evidence_line))
}

/// 1a: one effect-check metrics line for a before/after window: the labeled
/// `MetricsSummary` rendering, or "<label>: (no data)" when the agent has no
/// metric lines in that window.
pub(crate) fn window_metrics_line(
    label: &str,
    summary: &crate::agents::metrics::MetricsSummary,
) -> String {
    match summary.format_labeled(label) {
        line if line.is_empty() => format!("{label}: (no data)"),
        line => line,
    }
}

/// 2c: the evals half of the effect check — deterministic pass/fail in the same
/// before/after windows as the run metrics, so an eval *regression* is data the
/// improver can act on (not just LLM vibes over noisy run-metric samples).
/// Reuses the already-computed before/after `MetricsSummary` (its `evals*`
/// fields are aggregated over `MetricsLine::Eval`). Returns `None` when there
/// are no evals in either window (nothing to report).
pub(crate) fn evals_window_lines(
    before_start: chrono::DateTime<chrono::Utc>,
    since: chrono::DateTime<chrono::Utc>,
    before: &crate::agents::metrics::MetricsSummary,
    after: &crate::agents::metrics::MetricsSummary,
) -> Option<String> {
    if before.evals == 0 && after.evals == 0 {
        return None;
    }
    let mut s = format!(
        "Evals before the change ({} → {}): {} eval(s), {} passed, {} failed\n\
         Evals after the change ({} → now): {} eval(s), {} passed, {} failed",
        before_start.format("%Y-%m-%d"),
        since.format("%Y-%m-%d"),
        before.evals,
        before.evals_passed,
        before.evals.saturating_sub(before.evals_passed),
        since.format("%Y-%m-%d"),
        after.evals,
        after.evals_passed,
        after.evals.saturating_sub(after.evals_passed),
    );
    // Flag a regression: the after-window pass rate is strictly below the
    // before-window's, with evals in both (a deterministic signal, unlike runs).
    if before.evals > 0 && after.evals > 0 {
        let before_rate = before.evals_passed as f64 / before.evals as f64;
        let after_rate = after.evals_passed as f64 / after.evals as f64;
        if after_rate < before_rate {
            s.push_str(&format!(
                "\n⚠ eval regression: pass rate dropped from {:.0}% to {:.0}% since the change",
                before_rate * 100.0,
                after_rate * 100.0
            ));
        }
    }
    Some(s)
}

/// I1: one-line trajectory summary fed to the improver (and kept as evidence).
pub(crate) fn trajectory_line(stats: &RunStats, prompt_len: usize) -> String {
    format!(
        "Trajectory: {} tool calls ({} errors), {} verification attempt(s), \
         current system prompt {} chars.",
        stats.tool_calls, stats.tool_errors, stats.verification_attempts, prompt_len
    )
}

/// I1: cap the lessons section to `LESSON_CHAR_BUDGET` so a busy lesson store
/// can't blow out the analysis prompt. Truncates per-lesson first, then
/// drops whole lessons once the budget is spent.
pub(crate) fn cap_lessons(lessons: &[String]) -> String {
    let mut out = String::new();
    for lesson in lessons {
        let clipped: String = if lesson.chars().count() > 400 {
            let mut t: String = lesson.chars().take(400).collect();
            t.push('…');
            t
        } else {
            lesson.clone()
        };
        if !out.is_empty() {
            out.push('\n');
        }
        if out.len() + clipped.len() > LESSON_CHAR_BUDGET {
            break;
        }
        out.push_str(&clipped);
    }
    out
}
