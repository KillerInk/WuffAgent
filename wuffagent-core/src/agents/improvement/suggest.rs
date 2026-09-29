//! The single-agent improver: builds the analysis prompt from the agent's
//! lessons, run metrics, fleet context and effect evidence, calls the LLM,
//! and parses + sanitises the resulting suggestions.

use crate::agents::config::AgentConfig;
use crate::agents::RunStats;
use crate::llm::LlmClient;
use crate::memory::manager::MemoryManager;
use crate::types::{ImprovementSuggestion, Message};

use super::evidence::{cap_lessons, collect_lessons, effect_check_section, rejected_history, trajectory_line};
use super::{CHAT_PROFILE_NAME, NEWLINE, TOTAL_PROMPT_CHAR_BUDGET, fleet_summary_line, truncate_for_evidence};
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

/// 3b: the skill-RETIRE signal — one line listing the skills that EXIST in
/// the store but were never read in the usage window, so the improver can
/// propose `skill_updates` with `action: "delete"` for the ones with no
/// clear ongoing value. Pure (takes the store's listing + the used names),
/// so tests need no filesystem.
///
/// Complementary to the "skills read" line: that one lists skills with
/// recent usage, this one lists the rest. Empty string when every existing
/// skill was read in the window (or no skills exist at all).
pub(crate) fn skill_retire_line(all_skills: &[crate::memory::skills::SkillMeta], used: &[String], window_days: u64) -> String {
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
