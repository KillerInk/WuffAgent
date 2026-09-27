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
use wuffagent_core::memory::PendingStore;
use wuffagent_core::types::{
    ImprovementSuggestion, NewAgentProposal, ReasoningEffort, SkillUpdate,
};

use super::theme::Theme;

// Re-export for tests (`use super::*`); the non-test build has no external
// callers for these, so gate to avoid unused-import warnings.
#[cfg(test)]
pub use memory::{
    applied_marker, apply_improvement_detailed, rejection_lesson, remember_applied_prompt,
    remember_dismissal, resolve_agent_dir,
};
#[cfg(test)]
use std::path::PathBuf;
pub use wuffagent_core::agents::config::AgentConfig;
#[cfg(test)]
pub use wuffagent_core::memory::skills::SkillStore;

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
    /// 3c: per-skill "click again to confirm" armed state of the skill
    /// Revert buttons in the Skills section (canonical lowercase skill
    /// names; a proposal refresh clears it).
    pub skill_revert_armed: Vec<String>,
    /// 2c: proposed one-line description replacement (None = no change).
    pub description: Option<String>,
    /// I2: proposed profile-field changes (None = the LLM left them alone).
    pub allowed_tools: Option<Vec<String>>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub shell_config: Option<ShellConfig>,
    pub handoff_targets: Option<Vec<String>>,
    pub task_timeout_ms: Option<u64>,
    /// 3b: proposed skills (procedural memory) — saved via the shared
    /// SkillStore on approve (overwriting an existing name is the versioning
    /// mechanism).
    pub skill_updates: Vec<SkillUpdate>,
    /// I3: per-field approve toggles. Default true (apply the change); the
    /// user can untick any field to approve the rest without it.
    pub apply_prompt: bool,
    pub apply_description: bool,
    pub apply_allowed_tools: bool,
    pub apply_reasoning_effort: bool,
    pub apply_shell_config: bool,
    pub apply_handoff_targets: bool,
    pub apply_task_timeout: bool,
    pub apply_skills: bool,
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
            skill_revert_armed: Vec::new(),
            description: s.description.clone(),
            allowed_tools: s.allowed_tools.clone(),
            reasoning_effort: s.reasoning_effort,
            shell_config: s.shell_config.clone(),
            handoff_targets: s.handoff_targets.clone(),
            task_timeout_ms: s.task_timeout_ms,
            skill_updates: s.skill_updates.clone(),
            apply_prompt: true,
            apply_description: true,
            apply_allowed_tools: true,
            apply_reasoning_effort: true,
            apply_shell_config: true,
            apply_handoff_targets: true,
            apply_task_timeout: true,
            apply_skills: true,
            evidence: s.evidence.clone(),
        }
    }
}

/// A cached [`AgentManager::list_agents`] result plus the fingerprint that
/// produced it: the scanned directory set + the process-wide agent
/// mutation counter (bumped by every add/edit/remove/revert/reload,
/// including tool-driven ones like `edit_agent_profile`).
///
/// The panel used to call `list_agents()` on EVERY frame — a full disk
/// re-scan that re-reads and re-logs every agent profile, flooding the log
/// while tokens streamed (egui redraws on each streamed token). The list is
/// now refreshed only when an agent config was actually mutated or the dir
/// set changed (the per-frame check is a string compare — no I/O).
struct AgentListCache {
    fingerprint: String,
    agents: Vec<AgentConfig>,
}

/// Review panel state, owned by `ChatApp`.
pub struct ImprovementsPanel {
    pub pending: Vec<PendingImprovement>,
    pub show_panel: bool,
    pub message: Option<String>,
    /// G.1: where the pending queue is persisted (survives an app exit).
    /// `new()` creates the store but does NOT read from it — call
    /// [`Self::load_pending`] once at app startup to restore the queue.
    store: PendingStore,
    /// 4b: "run check now" button state — the selected agent profile, whether
    /// a check is in flight, and a one-line transient status message.
    run_check_agent: String,
    run_check_running: bool,
    run_check_status: String,
    /// Cached agent list (see [`AgentListCache`]); refreshed per frame only
    /// when the scanned agent dirs actually changed.
    agent_cache: Option<AgentListCache>,
}

impl ImprovementsPanel {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            show_panel: false,
            message: None,
            store: PendingStore::new(),
            // 4b: the selector UI re-resolves the default each frame if the
            // agent list is present, so an empty initial value is fine.
            run_check_agent: String::new(),
            run_check_running: false,
            run_check_status: String::new(),
            agent_cache: None,
        }
    }

    /// Re-fetch the cached agent list, but only when the fingerprint
    /// changed: a different dir set, or a bumped mutation counter (an agent
    /// config was added/edited/removed/reverted, via the UI or a tool).
    /// Cheap per-frame: no I/O, just a counter read + string compare.
    fn refresh_agent_cache(&mut self, manager: &AgentManager) {
        // The same dir set `list_agents()` scans: primary first, search
        // dirs after (skipping duplicates of the primary).
        let primary = manager.agents_dir().clone();
        let fingerprint = format!(
            "{}\0mut={}",
            std::iter::once(primary.to_string_lossy().into_owned())
                .chain(
                    manager
                        .search_dirs()
                        .iter()
                        .filter(|d| *d != &primary)
                        .map(|d| d.to_string_lossy().into_owned())
                )
                .collect::<Vec<_>>()
                .join("\0"),
            wuffagent_core::agents::manager::agents_mutation_counter()
        );
        let fresh = self
            .agent_cache
            .as_ref()
            .is_some_and(|c| c.fingerprint == fingerprint);
        if !fresh {
            let agents = manager.list_agents().unwrap_or_default();
            self.agent_cache = Some(AgentListCache {
                fingerprint,
                agents,
            });
        }
    }

    /// The cached agent list (empty until the first refresh).
    fn agents(&self) -> &[AgentConfig] {
        self.agent_cache
            .as_ref()
            .map(|c| c.agents.as_slice())
            .unwrap_or(&[])
    }

    /// 4b: a manual "run check now" check finished (see
    /// `AppEvent::ImprovementCheckFinished`). Clears the running state and
    /// records the outcome. Suggestions (when `produced`) arrive separately
    /// via [`Self::handle_improvement_suggested`], which also opens the panel.
    pub fn mark_check_finished(&mut self, agent_name: &str, produced: bool) {
        self.run_check_running = false;
        self.run_check_status = if produced {
            format!("Checked '{agent_name}' — suggestions added below.")
        } else {
            format!("Checked '{agent_name}' — no suggestions.")
        };
    }

    /// G.1: restore the pending queue persisted by a previous run (if any).
    /// A missing or corrupt file leaves the queue empty. Call once at
    /// startup, before any `handle_improvement_suggested`.
    pub fn load_pending(&mut self) {
        let loaded = self.store.load();
        self.pending = loaded.iter().map(PendingImprovement::from).collect();
        if !self.pending.is_empty() {
            self.show_panel = true;
        }
    }

    /// G.1: persist the current pending queue (user's in-panel edits win
    /// over the LLM's original text, so a reloaded queue shows what the
    /// user last typed). Call sites: after a new batch arrives and after
    /// items are removed. A disk failure only logs — the in-memory queue
    /// keeps working.
    fn persist(&self) {
        let items: Vec<ImprovementSuggestion> =
            self.pending.iter().map(suggestion_from_pending).collect();
        if let Err(err) = self.store.save(&items) {
            tracing::warn!(error = %err, "failed to persist pending improvement suggestions");
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
                existing.skill_revert_armed.clear();
                // I2/I3: refresh the proposed field changes + evidence; the
                // user's per-field toggles are preserved across the refresh.
                existing.description = s.description.clone();
                existing.allowed_tools = s.allowed_tools.clone();
                existing.reasoning_effort = s.reasoning_effort;
                existing.shell_config = s.shell_config.clone();
                existing.handoff_targets = s.handoff_targets.clone();
                existing.task_timeout_ms = s.task_timeout_ms;
                existing.skill_updates = s.skill_updates.clone();
                existing.evidence = s.evidence.clone();
            } else {
                self.pending.push(PendingImprovement::from(&s));
            }
        }
        self.show_panel = true;
        // G.1: the queue changed — persist it immediately (not only on
        // removal), so a crash/exit between suggestion and review loses
        // nothing.
        self.persist();
    }
}

/// G.1: inverse of `PendingImprovement::from` — collapse a pending UI item
/// back into the core suggestion for persistence. The user's in-panel
/// edits win over the LLM's original text (F1 "user edit wins"), so a
/// reloaded queue resumes where the user left off.
fn suggestion_from_pending(p: &PendingImprovement) -> ImprovementSuggestion {
    ImprovementSuggestion {
        agent_name: p.agent_name.clone(),
        prompt_change: p.edited_prompt.clone(),
        rationale: p.rationale.clone(),
        new_agents: p
            .new_agents
            .iter()
            .map(|na| NewAgentProposal {
                name: na.proposal.name.clone(),
                description: na.proposal.description.clone(),
                system_prompt: na.edited_system_prompt.clone(),
                allowed_tools: na.proposal.allowed_tools.clone(),
            })
            .collect(),
        description: p.description.clone(),
        allowed_tools: p.allowed_tools.clone(),
        reasoning_effort: p.reasoning_effort,
        shell_config: p.shell_config.clone(),
        handoff_targets: p.handoff_targets.clone(),
        task_timeout_ms: p.task_timeout_ms,
        skill_updates: p.skill_updates.clone(),
        evidence: p.evidence.clone(),
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
        let theme = Theme::from_name(&self.core.config.theme);
        // 4b: the "run check now" button needs a clonable memory handle and
        // the AppEvent channel to send back the result of the check it spawns.
        let memory_arc = self.core.memory_manager.clone();
        let events = self.relay.pending_tx.clone();
        self.dialogs.improvements_panel.draw(
            ctx,
            &agent_manager,
            &agents_dirs,
            &theme,
            &self.core.memory_manager,
            memory_arc,
            events,
        );
    }
}

#[cfg(test)]
mod tests;
