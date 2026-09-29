//! Improvement suggestions: analyze an agent's lessons + run trajectory and
//! propose prompt/config changes (or new agents). Lives in the agents brick
//! because its output types are the agents' profile fields; it reads the
//! memory store through `MemoryManager` (the memory brick keeps the trigger).

use super::config::AgentConfig;

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


mod evidence;
mod fleet;
mod policy;
mod suggest;

// Keep the crate::agents::improvement:: paths stable: the loop logic moved
// into the policy / evidence / suggest / fleet sub-files.
pub use fleet::*;
pub use policy::*;
pub use suggest::*;

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
