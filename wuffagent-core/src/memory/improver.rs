use serde::{Deserialize, Serialize};
use tracing;

use super::manager::MemoryManager;
use super::types::MemoryEntry;
use crate::agents::config::{AgentConfig, ShellConfig};
use crate::agents::RunStats;
use crate::llm::LlmClient;
use crate::types::{Message, ReasoningEffort};

/// I1: per-lesson character budget inside the improver prompt (keeps a
/// chatty lesson store from blowing out the analysis call).
const LESSON_CHAR_BUDGET: usize = 12_000;
/// I1: hard cap on the total improver prompt (safety net on top of the
/// lesson budget for large system prompts).
const TOTAL_PROMPT_CHAR_BUDGET: usize = 24_000;
/// Newline character (I5 effect-check glue; spelled out to keep the string
/// literals below readable).
const NEWLINE: char = '\u{a}';

/// A suggested improvement to an agent's configuration.
///
/// I2: beyond the prompt, the improver may now propose changes to any other
/// profile field. All new fields are optional and serde-defaulted, so old
/// suggestion JSON (and LLM responses that omit them) still parse.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImprovementSuggestion {
    pub agent_name: String,
    /// New prompt text, or None if no prompt change suggested.
    pub prompt_change: Option<String>,
    /// Explanation for why this improvement is suggested.
    pub rationale: String,
    /// Proposals for new specialized agents (LLMs commonly omit it when empty).
    #[serde(default)]
    pub new_agents: Vec<NewAgentProposal>,
    /// I2: replace the agent's tool allowlist (None = no change).
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// I2: change the agent's reasoning effort (None = no change).
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// I2: change the agent's shell configuration (None = no change).
    #[serde(default)]
    pub shell_config: Option<ShellConfig>,
    /// I2: change the agent's handoff target allowlist (None = no change).
    #[serde(default)]
    pub handoff_targets: Option<Vec<String>>,
    /// I2: change the per-task timeout in ms (None = no change).
    #[serde(default)]
    pub task_timeout_ms: Option<u64>,
    /// I3: the evidence the improver saw (trajectory line + lesson excerpts).
    /// Filled deterministically by `suggest_improvements`, not the LLM, so
    /// the review panel can show WHY the suggestion was made.
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// A proposal to create a new agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAgentProposal {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
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

    let section = format!(
        "The prompt for '{agent_name}' was last changed via an approved improvement {days} day(s) ago ({date}).
Outcomes recorded for this agent since that change:
{list}
If the outcomes look worse than before the change, you may propose reverting the prompt (prompt_change set to the previous prompt text) — the user can always revert from history."
    );
    let evidence_line = format!("Effect check: prompt last changed via approved improvement {days} day(s) ago ({date}); {} outcome(s) since", outcomes.len());
    Some((section, evidence_line))
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
    // I5: effect check — the last approved prompt change for this agent and
    // the outcomes recorded since it (None -> no section, prompt unchanged).
    let effect = effect_check_section(manager, &agent_config.name);
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
         Current system prompt:\n{}\n\
         \n\
         Recent task: {}\n\
         Result: {}\n\
         \n\
         {}\n\
         \n\
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
         \n\
         You may change the system prompt AND/OR any of these profile fields \
         (omit a field entirely when it needs no change):\n\
         - prompt_change (string or null)\n\
         - allowed_tools (array of tool names, or null)\n\
         - reasoning_effort (\"off\" | \"low\" | \"medium\" | \"high\", or null)\n\
         - shell_config ({{\"shell_enabled\": bool, \"allowed_commands\": [..], \"shell_timeout_ms\": number}}, or null)\n\
         - handoff_targets (array of agent names, or null)\n\
         - task_timeout_ms (number, or null)\n\
         \n\
         Return a JSON array of suggestions (empty [] if nothing to improve):\n\
         [\n\
           {{\n\
             \"agent_name\": \"{}\",\n\
             \"prompt_change\": \"new prompt text or null if no change needed\",\n\
             \"rationale\": \"why this change is needed\",\n\
             \"allowed_tools\": null,\n\
             \"reasoning_effort\": null,\n\
             \"shell_config\": null,\n\
             \"handoff_targets\": null,\n\
             \"task_timeout_ms\": null,\n\
             \"new_agents\": [\n\
               {{\"name\": \"agent_name\", \"description\": \"...\", \"system_prompt\": \"...\", \"allowed_tools\": [\"tool1\", \"tool2\"]}}\n\
             ]\n\
           }}\n\
         ]\n\
         \n\
         Return [] if no improvements are needed.",
        agent_config.name,
        agent_config.description,
        if prompt.is_empty() {
            format!("You are the '{}' agent. {}", agent_config.name, agent_config.description)
        } else {
            prompt
        },
        task,
        result,
        traj,
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

    let response = llm_client.complete(&messages).await?;

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
    for s in &mut suggestions {
        s.evidence = evidence.clone();
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
mod tests;
