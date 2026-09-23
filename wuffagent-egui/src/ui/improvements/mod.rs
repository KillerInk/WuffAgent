//! Self-improvement review panel.
//!
//! Receives [`wuffagent_core::memory::ImprovementSuggestion`] items (via
//! `AppEvent::ImprovementSuggested`) and lets the user approve or dismiss
//! them. Approving a prompt change updates the agent's JSON config in place;
//! approving a new-agent proposal creates the agent file.
//!
//! Split into:
//! - [`draw`] — the panel's `draw` implementation (impl on `ImprovementsPanel`);
//! - [`memory`] — applying improvements + the F5/I5 memory bookkeeping.

mod draw;
mod memory;

use eframe::egui;
use wuffagent_core::agents::config::{AgentManager, ShellConfig};
use wuffagent_core::types::ReasoningEffort;

use super::theme::Theme;

// Re-export for tests (`use super::*`); the non-test build has no external
// callers for these, so gate to avoid unused-import warnings.
#[cfg(test)]
pub use memory::{
    apply_improvement_detailed, applied_marker, rejection_lesson, remember_applied_prompt,
    remember_dismissal, resolve_agent_dir,
};
#[cfg(test)]
pub use wuffagent_core::agents::config::AgentConfig;
#[cfg(test)]
use std::path::PathBuf;

/// A pending suggestion in the review panel (UI-level wrapper around the core
/// `ImprovementSuggestion`).
#[derive(Clone, Debug)]
pub struct PendingImprovement {
    pub agent_name: String,
    /// Suggested replacement system prompt (if any).
    pub prompt_change: Option<String>,
    /// F1: the user's edited copy of the proposed prompt. `None` until the
    /// text box has been drawn at least once; `apply_improvement` prefers this
    /// over the original `prompt_change` so panel edits are not lost.
    pub edited_prompt: Option<String>,
    /// Why the LLM is proposing this.
    pub rationale: String,
    /// New agents bundled with this suggestion (F1: each carries an edit buffer).
    pub new_agents: Vec<PendingNewAgent>,
    /// F4: the "click again to confirm" armed state of the Revert button.
    /// First click arms it (label changes to a confirmation), second click
    /// reverts the agent to its latest snapshot and drops this pending item.
    pub revert_armed: bool,
    /// I2: proposed profile-field changes (None = the LLM left them alone).
    pub allowed_tools: Option<Vec<String>>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub shell_config: Option<ShellConfig>,
    pub handoff_targets: Option<Vec<String>>,
    pub task_timeout_ms: Option<u64>,
    /// I3: per-field approve toggles. Default true (apply the change); the
    /// user can untick any field to approve the rest without it.
    pub apply_prompt: bool,
    pub apply_allowed_tools: bool,
    pub apply_reasoning_effort: bool,
    pub apply_shell_config: bool,
    pub apply_handoff_targets: bool,
    pub apply_task_timeout: bool,
    /// I3: the evidence the improver saw (trajectory line + lesson excerpts),
    /// shown in the panel so the user can judge WHY the change was proposed.
    pub evidence: Vec<String>,
}

/// One new-agent proposal bundled with an improvement suggestion.
#[derive(Clone, Debug)]
pub struct PendingNewAgent {
    pub proposal: wuffagent_core::memory::NewAgentProposal,
    /// F1: user's edited copy of the proposed system prompt (initialized from
    /// the LLM text; `apply_improvement` prefers this over the proposal).
    pub edited_system_prompt: String,
}

impl From<&wuffagent_core::memory::ImprovementSuggestion> for PendingImprovement {
    fn from(s: &wuffagent_core::memory::ImprovementSuggestion) -> Self {
        PendingImprovement {
            agent_name: s.agent_name.clone(),
            prompt_change: s.prompt_change.clone(),
            edited_prompt: s.prompt_change.clone(),
            rationale: s.rationale.clone(),
            new_agents: s
                .new_agents
                .iter()
                .map(|p| PendingNewAgent {
                    proposal: p.clone(),
                    edited_system_prompt: p.system_prompt.clone(),
                })
                .collect(),
            revert_armed: false,
            allowed_tools: s.allowed_tools.clone(),
            reasoning_effort: s.reasoning_effort,
            shell_config: s.shell_config.clone(),
            handoff_targets: s.handoff_targets.clone(),
            task_timeout_ms: s.task_timeout_ms,
            apply_prompt: true,
            apply_allowed_tools: true,
            apply_reasoning_effort: true,
            apply_shell_config: true,
            apply_handoff_targets: true,
            apply_task_timeout: true,
            evidence: s.evidence.clone(),
        }
    }
}

/// Review panel state, owned by `ChatApp`.
pub struct ImprovementsPanel {
    pub pending: Vec<PendingImprovement>,
    pub show_panel: bool,
    pub message: Option<String>,
}

impl ImprovementsPanel {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            show_panel: false,
            message: None,
        }
    }

    /// Called when the app receives an `AppEvent::ImprovementSuggested`.
    pub fn handle_improvement_suggested(
        &mut self,
        agent_name: &str,
        suggestions: Vec<wuffagent_core::memory::ImprovementSuggestion>,
    ) {
        let _ = agent_name;
        // F2: APPEND instead of replacing — a new batch must not drop
        // previously unreviewed entries. Duplicates (same agent_name +
        // rationale) replace the existing entry in place so a re-suggestion
        // refreshes the text instead of stacking an identical row.
        for s in suggestions {
            if let Some(existing) = self
                .pending
                .iter_mut()
                .find(|p| p.agent_name == s.agent_name && p.rationale == s.rationale)
            {
                existing.prompt_change = s.prompt_change.clone();
                existing.edited_prompt = s.prompt_change.clone();
                existing.rationale = s.rationale.clone();
                existing.new_agents = s
                    .new_agents
                    .iter()
                    .map(|p| PendingNewAgent {
                        proposal: p.clone(),
                        edited_system_prompt: p.system_prompt.clone(),
                    })
                    .collect();
                existing.revert_armed = false;
                // I2/I3: refresh the proposed field changes + evidence; the
                // user's per-field toggles are preserved across the refresh.
                existing.allowed_tools = s.allowed_tools.clone();
                existing.reasoning_effort = s.reasoning_effort;
                existing.shell_config = s.shell_config.clone();
                existing.handoff_targets = s.handoff_targets.clone();
                existing.task_timeout_ms = s.task_timeout_ms;
                existing.evidence = s.evidence.clone();
            } else {
                self.pending.push(PendingImprovement::from(&s));
            }
        }
        self.show_panel = true;
    }
}

/// ChatApp extension: the improvements panel integration.
impl crate::ui::state::ChatApp {
    /// Build an AgentManager with the same discovery set the chat agent
    /// selector uses (`agents_dirs` in `input.rs`): the config-dir
    /// `agents/` as primary (new agents are written there), plus the
    /// project-level `agents/` dirs as search dirs.
    pub(super) fn build_agent_manager(&self) -> AgentManager {
        let dirs = self.agents_dirs();
        let mut manager = AgentManager::new(dirs[0].clone());
        for dir in &dirs[1..] {
            manager.add_search_dir(dir.clone());
        }
        manager
    }

    /// Draw the improvements panel.
    pub(super) fn draw_improvements_panel(&mut self, ctx: &egui::Context) {
        let agents_dirs = self.agents_dirs();
        let agent_manager = self.build_agent_manager();
        let theme = Theme::from_name(&self.config.theme);
        self.improvements_panel.draw(ctx, &agent_manager, &agents_dirs, &theme, &self.memory_manager);
    }
}

#[cfg(test)]
mod tests;
