//! Improvement suggestions: analyze an agent's lessons + run trajectory and
//! propose prompt/config changes (or new agents). Lives in the agents brick
//! because its output types are the agents' profile fields; it reads the
//! memory store through `MemoryManager` (the memory brick keeps the trigger).

use tracing;

use super::config::AgentConfig;
use super::RunStats;
use crate::llm::LlmClient;
use crate::memory::manager::MemoryManager;
use crate::memory::types::MemoryEntry;
use crate::types::Message;

/// I1: per-lesson character budget inside the improver prompt (keeps a
/// chatty lesson store from blowing out the analysis call).
const LESSON_CHAR_BUDGET: usize = 12_000;
/// I1: hard cap on the total improver prompt (safety net on top of the
/// lesson budget for large system prompts).
const TOTAL_PROMPT_CHAR_BUDGET: usize = 24_000;
/// Newline character (I5 effect-check glue; spelled out to keep the string
/// literals below readable).
const NEWLINE: char = '\u{a}';
/// The synthetic chat profile: the UI's default agent identity. It exists
/// in the metrics and the improver's run-check selector (where it is the
/// first entry) but has NO backing .json file in any agents dir — so an
/// improver suggestion targeting it must be a prompt change (approved
/// through the profile's chat settings), never a `new_agents` creation
/// (approving one would fail with "profile not found").
pub const CHAT_PROFILE_NAME: &str = "chat";

/// The synthetic [`AgentConfig`] for the chat profile: the identity the
/// improver needs (name + a base system prompt). Callers that hold the
/// live app config should overwrite `system_prompt` with the user's actual
/// chat system prompt - the static base text is what the tool-side check
/// (`run_self_improvement`) sees when the tool does not hold the config.
pub fn synthetic_chat_config() -> AgentConfig {
    AgentConfig {
        name: CHAT_PROFILE_NAME.to_string(),
        description: "General purpose agent (the chat UI's default profile; no backing file - configured through the chat settings)"
            .to_string(),
        system_prompt: "You are WuffAgent, a self-improving general-purpose agent.".to_string(),
        ..Default::default()
    }
}

// ImprovementSuggestion / NewAgentProposal live in the types brick (types
// embeds them in AppEvent::ImprovementSuggested); re-exported here so the
// crate::agents:: and crate::memory:: paths stay stable.
pub use crate::types::{ImprovementSuggestion, NewAgentProposal};

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

/// Gather relevant lesson memories for an improvement check.
///
/// Tag first (S3): lessons saved with the `agent:<name>` tag (the convention
/// the memory tool description and system-prompt hints ask for) are the
/// strongest per-agent signal. Fall back to the previous behavior — search by
/// agent name + task text, then the most recent lessons — when the tag search
/// finds nothing (backward compatible: old entries simply don't match the
/// tag).
fn collect_lessons(manager: &MemoryManager, agent_name: &str, task: &str) -> Vec<String> {
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
fn rejected_history(manager: &MemoryManager, agent_name: &str) -> Vec<String> {
    manager
        .get_by_tag("improvement-rejected")
        .into_iter()
        .filter(|m| m.tags.iter().any(|t| t == &format!("agent:{agent_name}")))
        .map(|m| m.content)
        .collect()
}

/// I5: the most recent "prompt changed via approved improvement" marker for
/// this agent (recorded by the review panel when the user approves a prompt
/// change; a Fact entry tagged `improvement-applied` + `agent:<name>`).
fn latest_applied_marker(manager: &MemoryManager, agent_name: &str) -> Option<MemoryEntry> {
    manager
        .get_by_tag("improvement-applied")
        .into_iter()
        .filter(|m| m.tags.iter().any(|t| t == &format!("agent:{agent_name}")))
        .max_by_key(|m| m.timestamp)
}

/// I5: this agent's recorded outcomes since `since` — Lesson entries tagged
/// `agent:<name>` (S1 verification verdicts, S2 user feedback, S3 agent
/// lessons). Facts (e.g. the I5 marker itself) are not outcomes.
fn outcomes_since(
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

/// I5: the "effect check" section of the improver prompt — how long ago the
/// last approved prompt change happened and which outcomes were recorded for
/// this agent since it, so the improver can propose a revert when the change
/// made things worse.
///
/// Returns `(prompt section, evidence line)`, or `None` when no approved
/// change has been recorded for this agent yet (the prompt gets no section).
fn effect_check_section(manager: &MemoryManager, agent_name: &str) -> Option<(String, String)> {
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
fn window_metrics_line(
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
fn evals_window_lines(
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
fn trajectory_line(stats: &RunStats, prompt_len: usize) -> String {
    format!(
        "Trajectory: {} tool calls ({} errors), {} verification attempt(s), \
         current system prompt {} chars.",
        stats.tool_calls, stats.tool_errors, stats.verification_attempts, prompt_len
    )
}

/// I1: cap the lessons section to `LESSON_CHAR_BUDGET` so a busy lesson store
/// can't blow out the analysis prompt. Truncates per-lesson first, then
/// drops whole lessons once the budget is spent.
fn cap_lessons(lessons: &[String]) -> String {
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

/// Suggest improvements for an agent based on its memory and recent task result.
///
/// Returns a list of suggestions. Empty list means no improvements needed.
/// I1: also takes the run's `RunStats` (trajectory) so the improver weighs
/// HOW the agent worked, not just the final text.
pub async fn suggest_improvements(
    manager: &MemoryManager,
    agent_config: &AgentConfig,
    task: &str,
    result: &str,
    stats: &RunStats,
    llm_client: &dyn LlmClient,
) -> Result<Vec<ImprovementSuggestion>, String> {
    if !manager.config().auto_improve {
        return Ok(Vec::new());
    }

    // Gather relevant lesson memories for this agent
    let lessons = collect_lessons(manager, &agent_config.name, task);

    if lessons.len() < manager.config().improvement_trigger_lessons {
        tracing::debug!(
            "No relevant lessons for agent '{}', skipping improvement check",
            agent_config.name
        );
        return Ok(Vec::new());
    }

    tracing::info!("[MEMORY] Improvement check for agent '{}': found {} relevant lesson(s), triggering LLM analysis", agent_config.name, lessons.len());

    let memories_text = cap_lessons(&lessons);
    let prompt = agent_config.system_prompt.clone();
    // I1: previously rejected suggestions (F5) as explicit negative evidence.
    let rejected = rejected_history(manager, &agent_config.name);
    let rejected_text = if rejected.is_empty() {
        "(none)".to_string()
    } else {
        rejected.join("\n")
    };
    // I1: trajectory line (also reused verbatim as evidence).
    let traj = trajectory_line(stats, prompt.chars().count());
    // M1: this agent's recent run metrics (default window, 2e) as outcome
    // evidence — the trajectory line above covers only THIS run; the metrics
    // cover the trend (error rate, verification outcomes, user feedback).
    let metrics_log = crate::agents::metrics::MetricsLog::default();
    let window_days = manager.config().improvement_metrics_window_days.max(1) as i64;
    let since7 = chrono::Utc::now() - chrono::Duration::days(window_days);
    let metrics_line = metrics_log.summary_since(&agent_config.name, Some(since7)).format_line();
    // 2b: fleet view — every other agent's windowed summary on one line each,
    // so the improver can see this agent's results in context of its siblings
    // (a bad handoff target, a sibling whose config is clearly working).
    let fleet_line = fleet_summary_line(window_days as u64);
    // 3b: cross-agent skill usage (read_skill calls, window) — does
    // procedural memory actually get used? Feeds the skill_updates signal.
    let skills_used = metrics_log.skill_usage_since(Some(since7));
    let skills_line = if skills_used.is_empty() {
        String::new()
    } else {
        let names = skills_used.iter().take(10).cloned().collect::<Vec<_>>();
        let more = skills_used.len() - names.len();
        let list = names.join(", ");
        if more > 0 {
            format!(
                "Skills read in the last {window_days} day(s) (all agents): {list} (+{more} more)"
            )
        } else {
            format!("Skills read in the last {window_days} day(s) (all agents): {list}")
        }
    };
    // 3b: the RETIRE signal — skills that exist in the store but were never
    // read in the window. `skills_line` only lists skills that WERE read, so
    // without this the improver can propose new/updated skills but never
    // prunes the ones that rotted. Computed via a pure helper (testable
    // without touching the default skills dir).
    let retire_line = skill_retire_line(
        &crate::memory::skills::SkillStore::default().list(),
        &skills_used,
        window_days as u64,
    );
    // 3b: splice the retire signal right after the "skills read" line — both
    // are the skill-maintenance evidence for `skill_updates` suggestions.
    let skills_and_retire = if retire_line.is_empty() {
        skills_line.clone()
    } else if skills_line.is_empty() {
        retire_line.clone()
    } else {
        format!("{skills_line}\n{retire_line}")
    };
    // I5: effect check — the last approved prompt change for this agent and
    // the outcomes recorded since it (None -> no section, prompt unchanged).
    let effect = effect_check_section(manager, &agent_config.name);
    // Chat profile: tell the LLM the profile EXISTS (so it proposes a
    // prompt_change for it, not a new_agents entry — approving a new agent
    // named "chat" would fail with "profile not found", and the duplicate
    // would shadow the real chat identity in the selector).
    let chat_note = if agent_config.name.eq_ignore_ascii_case(CHAT_PROFILE_NAME) {
        format!(
            "NOTE: The profile '{}' EXISTS as an agent profile (the chat agent). \
             It has no backing .json file — it is configured through the chat settings. \
             Propose prompt_change for agent_name \"{}\" to change it; do NOT list it in new_agents. \
             Its current prompt is the chat agent's system prompt shown above. \
             Its handoff targets (if any) are the real agent profiles the chat delegates to. \
             ",
            CHAT_PROFILE_NAME, CHAT_PROFILE_NAME
        )
    } else {
        String::new()
    };
    let effect_text = match &effect {
        Some((section, _)) => {
            let mut t = section.clone();
            t.push(NEWLINE);
            t.push(NEWLINE);
            t
        }
        None => String::new(),
    };

    let extraction_prompt = format!(
        "You are reviewing an AI agent's performance to suggest improvements.\n\n\
         Agent name: {}\n\
         Agent description: {}\n\
         {}\
         Current system prompt (FULL — prompt_change below is a FULL replacement of this text, not an edit or diff):\n{}\n\
         \n\
         Recent task: {}\n\
         Result: {}\n\
         \n\
         {}\n\
         {}\n\
         {}\n\
         \n\
         \n\
         {}\n\
         Previously rejected suggestions (do NOT re-suggest these):\n{}\n\
         \n\
         Relevant lesson memories:\n{}\n\
         \n\
         Analyze whether the agent's configuration should be improved.\n\
         Consider:\n\
         - What went well? What went wrong?\n\
         - Are there patterns in the lessons that suggest the prompt needs adjustment?\n\
         - Do the tool-call/error/verification numbers suggest a capability or\
         configuration problem (too many retries, repeated tool errors)?\n\
         - Is there a capability gap that would require a new specialized agent?\n\
          - Skill maintenance: if a skill on the \"NEVER read\" line above has no clear ongoing value, propose retiring it (skill_updates with action \"delete\"). If two existing skills overlap heavily, merge them (one \"update\" that absorbs the other's steps, plus a \"delete\" for the redundant one).\n\
         \n\
         You may change the system prompt AND/OR any of these profile fields \
         (omit a field entirely when it needs no change):\n\
         - prompt_change (string or null)\n\
         - description (short one-line description of the agent, or null)\n\
         - allowed_tools (array of tool names, or null)\n\
         - reasoning_effort (\"off\" | \"low\" | \"medium\" | \"high\", or null)\n\
         - shell_config ({{\"shell_enabled\": bool, \"allowed_commands\": [..], \"shell_timeout_ms\": number}}, or null)\n\
         - handoff_targets (array of agent names, or null)\n\
         - task_timeout_ms (number, or null)\n\
         - skill_updates (array of skill objects with action \"new\" | \"update\" | \"delete\", or null)\n\
         \n\
         Return a JSON array of suggestions (empty [] if nothing to improve):\n\
         [\n\
           {{\n\
             \"agent_name\": \"{}\",\n\
             \"prompt_change\": \"the FULL new system prompt text (replace the current one shown above) or null if no change needed\",\n\
             \"rationale\": \"why this change is needed\",\n\
             \"description\": null,\n\
             \"allowed_tools\": null,\n\
             \"reasoning_effort\": null,\n\
             \"shell_config\": null,\n\
             \"handoff_targets\": null,\n\
             \"task_timeout_ms\": null,\n\
             \"new_agents\": [\n\
         {{\"name\": \"agent_name\", \"description\": \"...\", \"system_prompt\": \"...\", \"allowed_tools\": [\"tool1\", \"tool2\"]}}\n\
         ],\n\
         \"skill_updates\": [\n\
             {{\"action\": \"new|update|delete\", \"name\": \"skill-slug\", \"description\": \"one line\", \"when_to_use\": \"when this skill applies\", \"body\": \"markdown steps\"}}\n\
             ]\n\
           }}\n\
         ]\n\
         \n\
         Return [] if no improvements are needed.",
        agent_config.name,
        agent_config.description,
        chat_note,
        if prompt.is_empty() {
            format!("You are the '{}' agent. {}", agent_config.name, agent_config.description)
        } else {
            prompt
        },
        task,
        result,
        traj,
        metrics_line,
        fleet_line,
        skills_and_retire,
        rejected_text,
        memories_text,
        agent_config.name,
    );

    // I5: splice the effect-check section in right before the lessons section
    // (first occurrence) so it counts toward the TOTAL_PROMPT_CHAR_BUDGET
    // truncation below.
    let extraction_prompt = if effect_text.is_empty() {
        extraction_prompt
    } else {
        let mut prompt = extraction_prompt;
        if let Some(idx) = prompt.find("Relevant lesson memories:") {
            prompt.insert_str(idx, &effect_text);
        }
        prompt
    };

    // I1: safety net — even with the lesson budget, a huge system prompt
    // could push the total past the cap; truncate the tail.
    let extraction_prompt: String = if extraction_prompt.len() > TOTAL_PROMPT_CHAR_BUDGET {
        let mut t: String = extraction_prompt
            .chars()
            .take(TOTAL_PROMPT_CHAR_BUDGET)
            .collect();
        t.push_str(" [truncated]");
        t
    } else {
        extraction_prompt
    };

    let messages = vec![Message {
        role: "user".to_string(),
        content: extraction_prompt,
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }];

    let check_start = std::time::Instant::now();
    let (response, check_usage) = llm_client.complete_with_usage(&messages).await?;
    let check_duration_ms = check_start.elapsed().as_millis() as u64;
    let (check_tokens_in, check_tokens_out) = check_usage
        .as_ref()
        .map(|u| (u.prompt_tokens as u64, u.completion_tokens as u64))
        .unwrap_or((0, 0));

    // Parse JSON response
    let trimmed = response.trim();
    let mut suggestions = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<ImprovementSuggestion>>(trimmed)
            .map_err(|e| format!("Failed to parse improvement suggestions: {}", e))?
    } else {
        // Try to find JSON in the response
        let start = trimmed.find('[').unwrap_or(0);
        let end = trimmed.rfind(']').unwrap_or(trimmed.len());
        let json = &trimmed[start..=end];
        serde_json::from_str::<Vec<ImprovementSuggestion>>(json)
            .map_err(|e| format!("Failed to parse improvement suggestions: {}", e))?
    };

    // 1c: record the loop's own cost for this check (best-effort — never
    // fails the check).
    crate::agents::metrics::record_check(
        &agent_config.name,
        "agent",
        check_tokens_in,
        check_tokens_out,
        suggestions.len(),
        check_duration_ms,
    );

    // I3: attach the evidence the improver actually saw (deterministic —
    // not the LLM's echo of it) so the review panel can show why.
    let mut evidence = vec![
        traj,
        format!(
            "{} lesson(s) informed this suggestion; first: {}",
            lessons.len(),
            lessons
                .first()
                .map(|l| truncate_for_evidence(l))
                .unwrap_or_default()
        ),
    ];
    // I5: the effect-check input is deterministic evidence too, so the panel
    // shows WHY a revert-style suggestion was made.
    if let Some((_, line)) = &effect {
        evidence.push(line.clone());
    }
    // M1: the metrics summary is deterministic evidence as well.
    if !metrics_line.is_empty() {
        evidence.push(metrics_line);
    }
    // 2b: the fleet line is deterministic evidence too.
    if !fleet_line.is_empty() {
        evidence.push(fleet_line);
    }
    // 3b: the skill-usage line is deterministic evidence too.
    if !skills_line.is_empty() {
        evidence.push(skills_line);
    }
    // 3b: the retire signal is evidence as well (it is what a
    // delete/merge skill_updates suggestion would be based on).
    if !retire_line.is_empty() {
        evidence.push(retire_line);
    }
    for s in &mut suggestions {
        s.evidence = evidence.clone();
        // Re-target guard: the LLM occasionally targets a name that has no
        // backing profile file and is not the profile being reviewed either
        // (e.g. "coder" while reviewing "wuffagent"). Approving such an item
        // fails with "profile not found in any agents directory — nothing was
        // written" even though the proposed change was based on the reviewed
        // profile's own lessons, metrics and effect evidence. Re-target the
        // change onto the profile being reviewed. The chat profile ("chat")
        // is EXEMPT: it is an existing (synthetic) profile — the prompt
        // declares it and the panel applies a prompt_change for it to the
        // chat settings — so a "chat" suggestion is kept, not re-targeted.
        if !s.agent_name.eq_ignore_ascii_case(&agent_config.name)
            && !s.agent_name.eq_ignore_ascii_case(CHAT_PROFILE_NAME)
        {
            tracing::info!(
                "Improvement for '{}' targets unknown profile '{}' — re-targeting to '{}'",
                agent_config.name,
                s.agent_name,
                agent_config.name
            );
            s.agent_name = agent_config.name.clone();
        }
    }

    if suggestions.is_empty() {
        tracing::debug!(
            "No improvements suggested for agent '{}'",
            agent_config.name
        );
    } else {
        tracing::info!(
            "Generated {} improvement suggestion(s) for agent '{}'",
            suggestions.len(),
            agent_config.name
        );
    }

    Ok(suggestions)
}

/// 2d: re-target suggestions whose `agent_name` does not match a profile in
/// the `roster` (case-insensitive) onto the best-matching roster entry by
/// name similarity (a Dice coefficient over the lowercased names, matching
/// the metrics file-name normalisation).
///
/// Rationale: the fleet review sees agent names from the metrics (which
/// include synthetic identities like "chat" — the UI's default chat profile
/// has no backing file in any agents dir). A suggestion targeting such a
/// name fails on approve with "profile not found in any agents directory —
/// nothing was written". The fleet prompt asks the LLM to name the profile
/// it changes, and the roster IS the set of real profiles, so a
/// near-miss name is almost certainly a spelling/case drift of a roster
/// entry (e.g. "Orchestrator" vs "orchestrator") — snap it to the real
/// one. Exact matches are left untouched.
fn sanitize_fleet_agent_names(suggestions: &mut [ImprovementSuggestion], roster: &[(String, String)]) {
    if suggestions.is_empty() || roster.is_empty() {
        return;
    }
    for s in suggestions.iter_mut() {
        let exact = roster
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(&s.agent_name))
            // The chat profile is an EXISTING (synthetic) profile — the
            // prompt declares it and the panel applies a prompt_change for
            // it to the chat settings — so a "chat" suggestion is kept, not
            // re-targeted onto a fuzzy-matched roster entry.
            || s.agent_name.eq_ignore_ascii_case(CHAT_PROFILE_NAME);
        if exact {
            continue;
        }
        // Best fuzzy match: highest Dice coefficient over the lowercased
        // names, ignoring separators (spaces, underscores, hyphens) so
        // "sub session" and "subsession" match. Requires a minimum
        // similarity (0.6) to avoid snapping an unrelated name onto a
        // random roster entry; otherwise the suggestion keeps its original
        // name (and fails on approve, as before).
        let norm = |s: &str| -> String {
            s.to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect()
        };
        let target = norm(&s.agent_name);
        let mut best: Option<(f64, String)> = None;
        for (n, _) in roster.iter() {
            let a = norm(n);
            if a.is_empty() || target.is_empty() {
                continue;
            }
            let score = dice(&a, &target);
            let is_better = best.as_ref().map_or(true, |(b, _)| score > *b);
            if is_better {
                best = Some((score, n.clone()));
            }
        }
        if let Some((score, name)) = best {
            if score >= 0.6 {
                tracing::info!(
                    "Fleet improvement targets unknown profile '{}' — re-targeting to roster entry '{}' (similarity {:.2})",
                    s.agent_name,
                    name,
                    score
                );
                s.agent_name = name;
            }
        }
    }
}

/// Dice coefficient over two strings (character-bag overlap), in [0, 1].
/// `dice("abc", "abc") == 1.0`; `dice("abc", "") == 0.0`.
fn dice(a: &str, b: &str) -> f64 {
    use std::collections::HashMap;
    let count = |s: &str| {
        let mut m: HashMap<char, usize> = HashMap::new();
        for c in s.chars() {
            *m.entry(c).or_insert(0) += 1;
        }
        m
    };
    let ca = count(a);
    let cb = count(b);
    let overlap: usize = ca
        .iter()
        .map(|(c, n)| (*n).min(cb.get(c).copied().unwrap_or(0)))
        .sum();
    let la: usize = ca.values().sum();
    let lb: usize = cb.values().sum();
    if la == 0 || lb == 0 {
        return 0.0;
    }
    2.0 * overlap as f64 / (la + lb) as f64
}

/// 2b: the cross-agent fleet summary — one short line per agent with at
/// least one run in the window (2e: `window_days` days; self-inclusive; the
/// per-agent metrics line above already covers the target in detail). Empty
/// string when no agent has recent metrics.
fn fleet_summary_line(window_days: u64) -> String {
    let log = crate::agents::metrics::MetricsLog::default();
    let since = chrono::Utc::now() - chrono::Duration::days(window_days as i64);
    let mut parts = Vec::new();
    for name in log.agent_names() {
        let s = log.summary_since(&name, Some(since));
        if s.runs == 0 {
            continue;
        }
        parts.push(format!(
            "{name}: {} run(s), {} tool error(s), {} gave_up, feedback {}/{}",
            s.runs, s.tool_errors, s.gave_up, s.feedback_up, s.feedback_down
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("Fleet metrics (last {window_days} day(s)): {}", parts.join(" | "))
    }
}

/// 2d: character budget for the fleet evidence block (~2k tokens — the
/// plan's cap for the cross-agent summary handed to the fleet improver).
const FLEET_EVIDENCE_CHAR_BUDGET: usize = 8_000;
/// 2d: per-agent cap on top-lesson excerpts in the fleet evidence block.
const FLEET_TOP_LESSONS: usize = 3;
/// 2d: character cap for one lesson excerpt in the fleet evidence block.
const FLEET_LESSON_CHARS: usize = 120;

/// 2d: fleet-wide cross-agent evidence — a SINGLE compact JSON block
/// (2b(b)): per-agent run metrics (runs, tool calls/errors, gave_up,
/// feedback, duration, tokens), the agent's newest tagged lessons, and
/// fleet skill usage (read in the window vs. exists-but-never-read).
///
/// Returns an empty string when there is no signal at all — no agent with
/// runs in the window, no agent-tagged lessons, no skill activity — in which
/// case there is nothing for a fleet review to judge.
pub fn fleet_evidence_json(
    manager: &MemoryManager,
    roster: &[(String, String)],
    window_days: u64,
) -> String {
    let window_days = window_days.max(1);
    let since = chrono::Utc::now() - chrono::Duration::days(window_days as i64);
    let log = crate::agents::metrics::MetricsLog::default();

    let mut agents: Vec<serde_json::Value> = Vec::new();
    for file_name in log.agent_names() {
        // Metrics file names are normalized (agent_file_name); map back to
        // the real profile name from the roster so the LLM can name the
        // agent in its suggestions (case-insensitive, first match).
        let display_name = roster
            .iter()
            .find(|(n, _)| crate::agents::metrics::agent_file_name(n) == file_name)
            .map(|(n, _)| n.clone())
            .unwrap_or(file_name);
        let plain_name = display_name;
        let name = plain_name.clone();
        let s = log.summary_since(&name, Some(since));
        // The chat profile has metrics (it is the UI's default agent identity)
        // but no backing profile file — tell the fleet improver that a
        // suggestion targeting it must be a prompt change, never a new_agents
        // creation (approving one would fail with "profile not found" and the
        // duplicate would shadow the real chat identity in the selector).
        let chat_note = if name.eq_ignore_ascii_case(CHAT_PROFILE_NAME) {
            format!(
                " (note: '{}' is the chat profile — it has no backing .json file; \
                 suggest prompt_change for it, never new_agents)",
                CHAT_PROFILE_NAME
            )
        } else {
            String::new()
        };
        // The agent's newest tagged lessons (capped) — the only per-agent
        // free-text content in the block besides the deterministic metrics.
        // (Tag lookup uses the PLAIN profile name: the chat_note suffix only
        // decorates the name shown in the JSON below.)
        let mut tagged: Vec<MemoryEntry> = manager
            .get_by_tag(&format!("agent:{plain_name}"))
            .into_iter()
            .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
            .collect();
        tagged.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        let lessons: Vec<String> = tagged
            .into_iter()
            .take(FLEET_TOP_LESSONS)
            .map(|m| truncate_to(&m.content, FLEET_LESSON_CHARS))
            .collect();
        if s.runs == 0 && lessons.is_empty() {
            continue;
        }
        let error_pct = if s.tool_calls == 0 {
            0.0
        } else {
            (s.tool_errors as f64 / s.tool_calls as f64 * 1_000.0).round() / 10.0
        };
        agents.push(serde_json::json!({
            "name": format!("{name}{chat_note}"),
            "runs": s.runs,
            "tool_calls": s.tool_calls,
            "tool_errors": s.tool_errors,
            "error_pct": error_pct,
            "gave_up": s.gave_up,
            "feedback_up": s.feedback_up,
            "feedback_down": s.feedback_down,
            "avg_duration_s": s.avg_duration_secs(),
            "tokens_in": s.tokens_in,
            "tokens_out": s.tokens_out,
            "top_lessons": lessons,
        }));
    }

    // Fleet skill usage (3a/3b data): what was read in the window, and what
    // exists in the store but was never read — the retire signal, as data.
    let skills_read: Vec<String> = log.skill_usage_since(Some(since)).into_iter().take(10).collect();
    let skills_never_read: Vec<String> = crate::memory::skills::SkillStore::default()
        .list()
        .into_iter()
        .map(|m| m.name)
        .filter(|n| !skills_read.iter().any(|u| u == n))
        .take(10)
        .collect();

    if agents.is_empty() && skills_read.is_empty() && skills_never_read.is_empty() {
        return String::new();
    }

    // Budget: lessons are the bulk of the block — drop them first, keep the
    // metrics (they are what a fleet review is FOR).
    let build = |with_lessons: bool| {
        let agents_json: Vec<serde_json::Value> = agents
            .iter()
            .map(|a| {
                if with_lessons {
                    a.clone()
                } else {
                    let mut a = a.clone();
                    a["top_lessons"] = serde_json::json!([]);
                    a
                }
            })
            .collect();
        serde_json::json!({
            "window_days": window_days,
            "agents": agents_json,
            "skills_read": skills_read,
            "skills_never_read": skills_never_read,
        })
        .to_string()
    };

    let mut json = build(true);
    if json.len() > FLEET_EVIDENCE_CHAR_BUDGET {
        json = build(false);
    }
    if json.len() > FLEET_EVIDENCE_CHAR_BUDGET {
        let mut t: String = json.chars().take(FLEET_EVIDENCE_CHAR_BUDGET).collect();
        t.push_str("… [truncated]");
        json = t;
    }
    json
}

/// 2d: hard character cap (the fleet evidence block's lesson excerpts).
fn truncate_to(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max_chars).collect();
        t.push('…');
        t
    }
}

/// 2d: the fleet-wide review (2b(b)) — the cross-agent counterpart of
/// `suggest_improvements`.
///
/// `roster` is the (name, description) of every known agent profile; the
/// caller holds the `AgentManager` (the `MemoryManager` does not know about
/// profiles). Evidence is the compact [`fleet_evidence_json`] block — a
/// single JSON summary (~≤2k tokens, 2d). The LLM looks for CROSS-agent
/// patterns a single-agent review would miss: the same failure across
/// several agents (→ a shared skill, or the per-agent change each needs), a
/// capability gap a new SHARED agent could fill (`new_agents`), and
/// fleet-wide skill maintenance.
///
/// The output is the SAME suggestion JSON as the per-agent path — each
/// suggestion names the agent it targets (`agent_name`) — so the review
/// panel and the `AppEvent::ImprovementSuggested` plumbing handle it
/// unchanged.
pub async fn suggest_fleet_improvements(
    manager: &MemoryManager,
    roster: &[(String, String)],
    focus: Option<&str>,
    llm_client: &dyn LlmClient,
) -> Result<Vec<ImprovementSuggestion>, String> {
    if !manager.config().auto_improve {
        tracing::debug!("auto_improve is off; skipping fleet improvement check");
        return Ok(Vec::new());
    }
    let window_days = manager.config().improvement_metrics_window_days.max(1) as u64;
    let evidence = fleet_evidence_json(manager, roster, window_days);
    if evidence.is_empty() {
        tracing::debug!("no fleet evidence in the window; skipping fleet improvement check");
        return Ok(Vec::new());
    }

    let roster_text = if roster.is_empty() {
        "(no agent profiles registered)".to_string()
    } else {
        let existing_list = roster
            .iter()
            .map(|(n, d)| {
                let d: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("{n} ({})", truncate_to(&d, 160))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let roster_block = roster
            .iter()
            .map(|(n, d)| {
                let d: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("{n}: {}", truncate_to(&d, 160))
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The synthetic chat profile exists (it is the UI's default identity,
        // configured through the chat settings) but has no backing .json file
        // in any agents dir — listing it among the real profiles would let the
        // LLM propose a NEW agent named "chat" (approving one would fail with
        // "profile not found" and the duplicate would shadow the real chat
        // identity in the selector).
        format!(
            "Existing agent profiles (do NOT propose these as new agents): {existing_list}. \
             Note: the chat profile ('{CHAT_PROFILE_NAME}') also exists — it is the UI's default \
             agent, configured through the chat settings; it has no backing .json file, so a \
             suggestion for it must be a prompt_change (never a new_agents entry). \
             \n\nRoster (name: description):\n{roster_block}"
        )
    };

    let focus_line = focus
        .map(|f| format!("\nConcentrate the review on: {f}"))
        .unwrap_or_default();

    let prompt = format!(
        "You are reviewing a FLEET of AI agent profiles in WuffAgent. Look for CROSS-AGENT \
         patterns that a single-agent review would miss.\n\n\
         Known agent profiles:\n{roster_text}\n\
         {focus_line}\n\n\
         Fleet evidence (last {window_days} day(s), single JSON block — per-agent run metrics, \
         tool error rates, durations, tokens, top lessons, plus skill usage):\n{evidence}\n\n\
         What to look for:\n\
         1. The SAME failure pattern across several agents (repeated tool errors, same misbehavior) → \
         propose a shared skill (skill_updates) capturing the fix, and/or one suggestion per \
         affected agent (its agent_name set) with the prompt/tool change it needs.\n\
         2. A capability gap that recurs across agents → propose a NEW SHARED agent (new_agents) \
         the fleet can hand off to.\n         2b. The chat profile ('{CHAT_PROFILE_NAME}') has no backing profile file - target it with \n         prompt_change, never new_agents (its run metrics may make it look like an existing profile; \n         approving a new 'chat' agent would fail and shadow the real chat identity).\n\
         3. Skills that exist but were never read fleet-wide (skills_never_read) → propose \
         retiring (action \"delete\") the ones with no clear ongoing value; merge heavily \
         overlapping skills.\n\
         4. An agent clearly outperforming its siblings → suggest what the others could borrow \
         (prompt style, tool allowlist).\n\n\
         Rules:\n\
         - Prefer few, high-confidence suggestions. If nothing rises above the noise, return an \
         empty array [].\n\n\
         Return a JSON array of suggestions (same shape as a single-agent review; empty [] if \
         nothing to improve):\n\
         [\n\
           {{\n\
             \"agent_name\": \"<profile to change>\",\n\
             \"prompt_change\": \"the FULL new system prompt text (replace the current one shown above) or null if no change needed\",\n\
             \"rationale\": \"why this change is needed (cite the fleet evidence)\",\n\
             \"description\": null,\n\
             \"allowed_tools\": null,\n\
             \"reasoning_effort\": null,\n\
             \"shell_config\": null,\n\
             \"handoff_targets\": null,\n\
             \"task_timeout_ms\": null,\n\
             \"new_agents\": [\n\
               {{\"name\": \"agent_name\", \"description\": \"...\", \"system_prompt\": \"...\", \"allowed_tools\": [\"tool1\", \"tool2\"]}}\n\
             ],\n\
             \"skill_updates\": [\n\
               {{\"action\": \"new|update|delete\", \"name\": \"skill-slug\", \"description\": \"one line\", \"when_to_use\": \"when this skill applies\", \"body\": \"markdown steps\"}}\n\
             ]\n\
           }}\n\
         ]\n\
         \n\
         Return [] if no improvements are needed.",
    );

    let messages = vec![Message {
        role: "user".to_string(),
        content: prompt,
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }];

    let check_start = std::time::Instant::now();
    let (response, check_usage) = llm_client.complete_with_usage(&messages).await?;
    let check_duration_ms = check_start.elapsed().as_millis() as u64;
    let (check_tokens_in, check_tokens_out) = check_usage
        .as_ref()
        .map(|u| (u.prompt_tokens as u64, u.completion_tokens as u64))
        .unwrap_or((0, 0));

    // Parse (same tolerant JSON extraction as the per-agent path).
    let trimmed = response.trim();
    let mut suggestions = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<ImprovementSuggestion>>(trimmed)
            .map_err(|e| format!("Failed to parse fleet improvement suggestions: {}", e))?
    } else {
        let start = trimmed.find('[').unwrap_or(0);
        let end = trimmed.rfind(']').unwrap_or(trimmed.len());
        let json = &trimmed[start..=end];
        serde_json::from_str::<Vec<ImprovementSuggestion>>(json)
            .map_err(|e| format!("Failed to parse fleet improvement suggestions: {}", e))?
    };

    // 1c: record the loop's own cost for this fleet-wide check (best-effort —
    // never fails the check).
    crate::agents::metrics::record_check(
        crate::agents::metrics::FLEET_FILE_STEM,
        "fleet",
        check_tokens_in,
        check_tokens_out,
        suggestions.len(),
        check_duration_ms,
    );

    // I3-style: attach the deterministic evidence (the fleet block itself,
    // display-truncated) so the panel shows WHY each suggestion was made.
    let evidence_lines = vec![truncate_for_evidence(&evidence)];
    for s in &mut suggestions {
        s.evidence = evidence_lines.clone();
    }

    // Synthetic-profile guard (fleet variant): the LLM occasionally names a
    // profile that exists only in the metrics (e.g. "chat" — the UI's
    // default identity, which has no backing file) or drifts on the
    // spelling/case of a roster entry. Re-target those onto the nearest
    // roster entry so approve can find a real profile to write.
    sanitize_fleet_agent_names(&mut suggestions, roster);

    if suggestions.is_empty() {
        tracing::debug!("No improvements suggested by the fleet review");
    } else {
        tracing::info!(
            "Generated {} improvement suggestion(s) from the fleet review",
            suggestions.len()
        );
    }
    Ok(suggestions)
}

/// 3b: the skill-RETIRE signal — one line listing the skills that EXIST in
/// the store but were never read in the usage window, so the improver can
/// propose `skill_updates` with `action: "delete"` for the ones with no
/// clear ongoing value. Pure (takes the store's listing + the used names),
/// so tests need no filesystem.
///
/// Complementary to the "skills read" line: that one lists skills with
/// recent usage, this one lists the rest. Empty string when every existing
/// skill was read in the window (or no skills exist at all).
fn skill_retire_line(all_skills: &[crate::memory::skills::SkillMeta], used: &[String], window_days: u64) -> String {
    let unused: Vec<&String> = all_skills
        .iter()
        .map(|m| &m.name)
        .filter(|n| !used.iter().any(|u| u.as_str() == n.as_str()))
        .collect();
    if unused.is_empty() {
        return String::new();
    }
    let names: Vec<String> = unused.iter().take(10).map(|s| s.to_string()).collect();
    let more = unused.len().saturating_sub(names.len());
    let list = names.join(", ");
    let tail = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    format!(
        "Skills that exist but were NEVER read in the last {window_days} day(s): {list}{tail} — \
         propose retiring (skill_updates action \"delete\") the ones with no clear ongoing value; \
         if two existing skills overlap heavily, merge them (one \"update\" that absorbs the other's \
         steps + a \"delete\" for the redundant one)."
    )
}

/// I3: keep evidence strings short (they are displayed in the review panel).
fn truncate_for_evidence(s: &str) -> String {
    if s.chars().count() > 300 {
        let mut t: String = s.chars().take(300).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests;
