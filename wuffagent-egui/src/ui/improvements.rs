//! Self-improvement review panel.
//!
//! Receives [`wuffagent_core::memory::ImprovementSuggestion`] items (via
//! `AppEvent::ImprovementSuggested`) and lets the user approve or dismiss
//! them. Approving a prompt change updates the agent's JSON config in place;
//! approving a new-agent proposal creates the agent file.

use std::path::PathBuf;

use eframe::egui;
use wuffagent_core::agents::config::{AgentConfig, AgentManager, ShellConfig, WorkerConfig};
use wuffagent_core::types::ReasoningEffort;

use super::theme::Theme;
use crate::memory::{MemoryEntry, MemoryManager, MemoryType};

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
    pub fn handle_improvement_suggested(&mut self, agent_name: &str, suggestions: Vec<wuffagent_core::memory::ImprovementSuggestion>) {
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

    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        agent_manager: &AgentManager,
        agents_dirs: &[PathBuf],
        theme: &Theme,
        memory: &MemoryManager,
    ) {
        if !self.show_panel || self.pending.is_empty() {
            return;
        }

        egui::Window::new("Self-Improvement Suggestions")
            .id(egui::Id::new("improvements_panel"))
            .default_size([520.0, 360.0])
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                let badge = egui::RichText::new(format!("({} pending)", self.pending.len()))
                    .strong()
                    .color(theme.accent);
                ui.horizontal(|ui| {
                    ui.heading("Self-Improvement Suggestions");
                    ui.label(badge);
                });
                ui.separator();

                if let Some(msg) = &self.message {
                    let color = if msg.contains("Error") { theme.error } else { theme.success };
                    ui.colored_label(color, msg);
                    ui.separator();
                }

                ui.label(egui::RichText::new("Review the LLM's proposed changes. Approve to apply, dismiss to reject.").weak());
                ui.separator();

                ui.label(egui::RichText::new("Pending suggestions:").strong());
                ui.add_space(4.0);

                // F4: per-item prompt history (newest first) for the Revert
                // button — only items proposing a prompt change for an
                // existing agent can be reverted; new-agent items have none.
                let item_histories: Vec<Vec<super::agent_history::HistoryEntry>> = self
                    .pending
                    .iter()
                    .map(|p| {
                        if p.prompt_change.is_some() {
                            super::agent_history::list_history(agents_dirs, &p.agent_name)
                        } else {
                            Vec::new()
                        }
                    })
                    .collect();

                // Collect actions to execute AFTER the loop (avoids mutating
                // `self.pending` while iterating).
                let mut to_remove: Vec<usize> = Vec::new();
                let mut to_approve: Vec<usize> = Vec::new();
                let mut to_revert: Vec<usize> = Vec::new();
                let mut to_dismiss: Vec<usize> = Vec::new();

                for (i, imp) in self.pending.iter_mut().enumerate() {
                    ui.collapsing(format!("Agent: {}", imp.agent_name), |ui| {
                        ui.label(egui::RichText::new(format!("Rationale: {}", imp.rationale)).weak());
                        ui.add_space(4.0);

                        // I3: show the evidence that triggered the suggestion.
                        if !imp.evidence.is_empty() {
                            ui.collapsing("Evidence (what the improver saw)", |ui| {
                                for e in &imp.evidence {
                                    ui.label(egui::RichText::new(e).weak().size(11.0));
                                }
                            });
                        }

                        if let Some(new_prompt) = &imp.prompt_change {
                            // I3: old-vs-new side by side — current prompt
                            // (read-only) next to the proposed one (editable).
                            let current_prompt = agent_manager
                                .get_agent(&imp.agent_name)
                                .map(|c| c.system_prompt.clone());
                            ui.horizontal(|ui| {
                                if let Some(cur) = &current_prompt {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Current (read-only)")
                                                .strong()
                                                .weak(),
                                        );
                                        egui::ScrollArea::vertical()
                                            .max_height(150.0)
                                            .show(ui, |ui| {
                                                ui.label(
                                                    egui::RichText::new(cur)
                                                        .monospace()
                                                        .size(11.0),
                                                );
                                            });
                                    });
                                    ui.separator();
                                }
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(if current_prompt.is_some() {
                                            "Proposed (editable)"
                                        } else {
                                            "Proposed system prompt (editable)"
                                        })
                                        .strong(),
                                    );
                                    let mut buf = imp
                                        .edited_prompt
                                        .as_deref()
                                        .unwrap_or(new_prompt)
                                        .to_string();
                                    ui.add(
                                        egui::TextEdit::multiline(&mut buf)
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(6),
                                    );
                                    // F1: persist the edited value in place so
                                    // the user's changes survive across frames
                                    // and are what gets applied on Approve.
                                    imp.edited_prompt = Some(buf);
                                });
                            });
                            ui.checkbox(
                                &mut imp.apply_prompt,
                                "Apply prompt change",
                            );
                        }

                        for na in imp.new_agents.iter_mut() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(format!("New agent: '{}' — {}", na.proposal.name, na.proposal.description)).strong());
                            let mut sp = na.edited_system_prompt.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut sp)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(4),
                            );
                            // F1: persist the edited value (see above).
                            na.edited_system_prompt = sp;
                        }

                        // I2/I3: per-field changes with approve toggles — the
                        // user can accept the prompt but reject a tool change
                        // (or vice versa). Only fields the LLM proposed show.
                        if let Some(tools) = &imp.allowed_tools {
                            ui.checkbox(
                                &mut imp.apply_allowed_tools,
                                format!("Apply tool allowlist change ({} tools: {})", tools.len(), tools.join(", ")),
                            );
                        }
                        if let Some(re) = &imp.reasoning_effort {
                            ui.checkbox(
                                &mut imp.apply_reasoning_effort,
                                format!("Apply reasoning effort change ({:?})", re),
                            );
                        }
                        if let Some(sc) = &imp.shell_config {
                            let desc = if sc.shell_enabled {
                                format!("shell enabled, {} command pattern(s)", sc.allowed_commands.len())
                            } else {
                                "shell disabled".to_string()
                            };
                            ui.checkbox(
                                &mut imp.apply_shell_config,
                                format!("Apply shell config change ({})", desc),
                            );
                        }
                        if let Some(ht) = &imp.handoff_targets {
                            ui.checkbox(
                                &mut imp.apply_handoff_targets,
                                format!("Apply handoff target change ({})", ht.join(", ")),
                            );
                        }
                        if let Some(ms) = imp.task_timeout_ms {
                            ui.checkbox(
                                &mut imp.apply_task_timeout,
                                format!("Apply task timeout change ({} ms)", ms),
                            );
                        }

                        let has_config_change = imp.prompt_change.is_some()
                            || imp.allowed_tools.is_some()
                            || imp.reasoning_effort.is_some()
                            || imp.shell_config.is_some()
                            || imp.handoff_targets.is_some()
                            || imp.task_timeout_ms.is_some();
                        if has_config_change {
                            // Existing-agent prompt change → Approve + Dismiss + Revert.
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(true, egui::Button::new("✓ Approve").fill(theme.primary))
                                    .clicked()
                                {
                                    if !to_approve.contains(&i) {
                                        to_approve.push(i);
                                    }
                                }
                                if ui.add(egui::Button::new("✗ Dismiss")).clicked() {
                                    if !to_remove.contains(&i) {
                                        to_remove.push(i);
                                    }
                                    if !to_dismiss.contains(&i) {
                                        to_dismiss.push(i);
                                    }
                                }
                                // F4: revert this agent to its latest prompt
                                // snapshot. "Click again to confirm" — no
                                // modal, keeps the per-frame draw simple.
                                let hist = &item_histories[i];
                                let has_hist = !hist.is_empty();
                                let label = if imp.revert_armed && has_hist {
                                    format!(
                                        "↩ Confirm revert to {}?",
                                        super::agent_history::format_ts(hist[0].ts)
                                    )
                                } else {
                                    "↩ Revert".to_string()
                                };
                                if ui
                                    .add_enabled(has_hist, egui::Button::new(label))
                                    .on_hover_text("Restore the previous prompt from the latest history snapshot. Pending suggestions for this agent are dropped (they were reviewed against the now-reverted prompt).")
                                    .clicked()
                                {
                                    if imp.revert_armed {
                                        to_revert.push(i);
                                    } else {
                                        imp.revert_armed = true;
                                    }
                                }
                                if !has_hist {
                                    ui.label(egui::RichText::new("(no prompt history)").weak());
                                }
                            });
                        } else {
                            // New-agent proposal → only dismiss makes sense
                            // (approve applies every bundled proposal).
                            ui.horizontal(|ui| {
                                if ui.add(egui::Button::new("✗ Dismiss")).clicked() {
                                    if !to_remove.contains(&i) {
                                        to_remove.push(i);
                                    }
                                    if !to_dismiss.contains(&i) {
                                        to_dismiss.push(i);
                                    }
                                }
                            });
                        }
                        ui.separator();
                    });
                }

                // F4: execute reverts (file I/O) before approves so both see
                // the pre-removal list; pending-list removals happen only in
                // the single pass below, so approve indices stay valid.
                for i in &to_revert {
                    let name = self.pending[*i].agent_name.clone();
                    let entry = item_histories[*i].first().cloned();
                    match entry {
                        Some(e) => match super::agent_history::revert(&e.dir, &name, &e) {
                            Ok(_) => {
                                self.message = Some(format!(
                                    "reverted '{}' to {} — pending suggestions for this agent were dropped",
                                    name,
                                    super::agent_history::format_ts(e.ts)
                                ));
                                // Drop ALL pending suggestions for this agent:
                                // they were reviewed against the now-reverted prompt.
                                for k in 0..self.pending.len() {
                                    if self.pending[k].agent_name == name && !to_remove.contains(&k) {
                                        to_remove.push(k);
                                    }
                                }
                            }
                            Err(err) => {
                                self.pending[*i].revert_armed = false;
                                self.message = Some(format!(
                                    "Error: revert of '{}' failed: {}",
                                    name, err
                                ));
                            }
                        },
                        None => {
                            self.pending[*i].revert_armed = false;
                            self.message = Some(format!(
                                "revert of '{}': no history snapshot available",
                                name
                            ));
                        }
                    }
                }

                // Execute collected actions (file I/O + list mutation) after
                // the loop so we never mutate while iterating.
                for i in to_approve.into_iter().rev() {
                    let (outcome, prompt_applied) =
                        apply_improvement_detailed(agents_dirs, agent_manager, &self.pending[i]);
                    self.message = Some(outcome);
                    // I5: an approved prompt change gets a marker so the next
                    // improvement check can weigh the outcomes since it and
                    // propose a revert. A store failure is surfaced (F5-style)
                    // but does not undo the approval itself.
                    if prompt_applied {
                        if let Err(err) = remember_applied_prompt(memory, &self.pending[i]) {
                            self.message = Some(format!(
                                "approved '{}', but remembering the prompt change failed: {}",
                                self.pending[i].agent_name, err
                            ));
                        }
                    }
                    if !to_remove.contains(&i) {
                        to_remove.push(i);
                    }
                }

                // F5: remember explicit dismissals as negative evidence for
                // the improver. The dedup gate in `MemoryManager::add` makes
                // re-dismissing the same suggestion a no-op. Approves and
                // reverts do NOT record a lesson (the item was acted on, not
                // rejected).
                for i in &to_dismiss {
                    if let Err(err) = remember_dismissal(memory, &self.pending[*i]) {
                        self.message = Some(format!(
                            "dismissed '{}', but remembering the rejection failed: {}",
                            self.pending[*i].agent_name, err
                        ));
                    }
                }

                for i in to_remove.into_iter().rev() {
                    if i < self.pending.len() {
                        self.pending.remove(i);
                    }
                }
            });
    }
}

/// F3: find the directory (in priority order) that ACTUALLY contains a
/// profile named `name`: a `.json` file that parses as `AgentConfig` or
/// legacy `WorkerConfig` with a matching `name` field — the same discovery
/// rules as `AgentManager::load_from_dir`, minus the `enabled` filter, so a
/// disabled profile can still be edited.
///
/// Returns `None` when no known directory holds the profile (it was deleted,
/// or it has no backing file at all — "synthetic").
pub fn resolve_agent_dir(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(cfg) = serde_json::from_str::<AgentConfig>(&content) {
                if cfg.name == name {
                    return Some(dir.clone());
                }
            } else if let Ok(legacy) = serde_json::from_str::<WorkerConfig>(&content) {
                if legacy.name == name {
                    return Some(dir.clone());
                }
            }
        }
    }
    None
}

/// Apply a single pending improvement (F1: uses the user-edited values when
/// present; F3: writes the profile into the directory it ACTUALLY lives in).
/// Returns `(human-readable result message, prompt_applied)` where
/// `prompt_applied` reports whether a prompt change for the EXISTING agent's
/// profile was successfully written (I5: the condition for recording the
/// effect-check marker — field-only approvals and new-agent-only approvals
/// report `false`).
pub fn apply_improvement_detailed(
    agents_dirs: &[PathBuf],
    agent_manager: &AgentManager,
    imp: &PendingImprovement,
) -> (String, bool) {
    let mut parts: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut prompt_applied = false;

    let apply_prompt = imp.apply_prompt
        && imp.edited_prompt.as_deref().or(imp.prompt_change.as_deref()).is_some();
    // I2: the profile fields the user actually approved (None = not proposed
    // or not toggled on).
    let do_tools = imp.apply_allowed_tools.then(|| imp.allowed_tools.clone()).flatten();
    let do_reasoning = imp.apply_reasoning_effort.then(|| imp.reasoning_effort).flatten();
    let do_shell = imp.apply_shell_config.then(|| imp.shell_config.clone()).flatten();
    let do_handoff = imp.apply_handoff_targets.then(|| imp.handoff_targets.clone()).flatten();
    let do_timeout = imp.apply_task_timeout.then(|| imp.task_timeout_ms).flatten();
    let has_field_change =
        do_tools.is_some() || do_reasoning.is_some() || do_shell.is_some() || do_handoff.is_some() || do_timeout.is_some();

    if apply_prompt || has_field_change {
        // F3: locate the profile's ACTUAL directory (primary first) and edit
        // the file in place there, via a manager bound to that directory —
        // which also puts the F4 history snapshot next to the profile.
        // Writing to the primary dir unconditionally would create a shadow
        // copy when the profile lives in a search dir.
        match resolve_agent_dir(agents_dirs, &imp.agent_name) {
            Some(dir) => {
                let mgr = AgentManager::new(dir.clone());
                match mgr.get_agent(&imp.agent_name) {
                    Some(mut config) => {
                        let mut applied: Vec<String> = Vec::new();
                        if apply_prompt {
                            config.system_prompt =
                                imp.edited_prompt.as_deref().or(imp.prompt_change.as_deref()).unwrap().to_string();
                            applied.push("prompt".to_string());
                        }
                        if let Some(tools) = do_tools {
                            config.allowed_tools = tools;
                            applied.push("tools".to_string());
                        }
                        if let Some(re) = do_reasoning {
                            config.reasoning_effort = re;
                            applied.push("reasoning".to_string());
                        }
                        if let Some(sc) = do_shell {
                            config.shell_config = sc;
                            applied.push("shell".to_string());
                        }
                        if let Some(ht) = do_handoff {
                            config.handoff_targets = ht;
                            applied.push("handoff".to_string());
                        }
                        if let Some(ms) = do_timeout {
                            config.task_timeout_ms = ms;
                            applied.push("timeout".to_string());
                        }
                        match mgr.edit_agent(&imp.agent_name, &config) {
                            Ok(()) => {
                                // I5: a prompt was actually written to the
                                // profile -> the effect-check marker applies.
                                prompt_applied =
                                    prompt_applied || applied.iter().any(|a| a == "prompt");
                                parts.push(format!(
                                    "updated {} for '{}' in {}",
                                    applied.join(", "),
                                    imp.agent_name,
                                    dir.display()
                                ));
                            }
                            Err(e) => errors.push(format!(
                                "failed to update '{}': {}",
                                imp.agent_name, e
                            )),
                        }
                    }
                    None => errors.push(format!(
                        "agent '{}': profile file found in {} but could not be loaded",
                        imp.agent_name,
                        dir.display()
                    )),
                }
            }
            None => errors.push(format!(
                "profile '{}' not found in any agents directory ({}); the profile may have been deleted or it has no backing file — nothing was written",
                imp.agent_name,
                agents_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    for na in &imp.new_agents {
        let mut proposal = na.proposal.clone();
        // F1: the user's edit wins over the LLM text.
        proposal.system_prompt = na.edited_system_prompt.clone();
        let config = AgentConfig {
            name: proposal.name.clone(),
            description: proposal.description.clone(),
            system_prompt: proposal.system_prompt.clone(),
            allowed_tools: proposal.allowed_tools.clone(),
            ..Default::default()
        };
        match agent_manager.add_agent(&config) {
            Ok(()) => parts.push(format!("created new agent '{}'", proposal.name)),
            Err(e) => errors.push(format!(
                "failed to create agent '{}': {}",
                proposal.name, e
            )),
        }
    }

    let msg = if parts.is_empty() && errors.is_empty() {
        "No changes to apply.".to_string()
    } else {
        let mut msg = parts.join("; ");
        if !errors.is_empty() {
            if !msg.is_empty() {
                msg.push_str("; ");
            }
            msg.push_str(&errors.join("; "));
        }
        msg
    };
    (msg, prompt_applied)
}

/// F5: the lesson entry recording that the user rejected `imp`.
///
/// Tagged `improvement-rejected` + `agent:<name>` and worded to name the
/// agent, so `collect_lessons` (improver.rs) finds it through its
/// agent-name+task keyword search and the improver sees the rejection as
/// negative evidence ("do not re-suggest the same change").
pub fn rejection_lesson(imp: &PendingImprovement) -> MemoryEntry {
    let content = format!(
        "Improvement for '{}' rejected by the user: {} — do not re-suggest the same change.",
        imp.agent_name, imp.rationale
    );
    let agent_tag = format!("agent:{}", imp.agent_name);
    MemoryEntry::new(
        MemoryType::Lesson,
        &content,
        "improvement-review",
        &["improvement-rejected", &agent_tag],
    )
}

/// F5: persist the dismissal of `imp` through the shared memory store.
///
/// Goes through the same save path (`MemoryManager::add`) the memory tools
/// use, so the dedup gate collapses a repeated dismissal of the same
/// suggestion. `Ok(())` on both insert and duplicate; `Err` only on store
/// failure (e.g. the memories file could not be written).
pub fn remember_dismissal(memory: &MemoryManager, imp: &PendingImprovement) -> Result<(), String> {
    memory.add(rejection_lesson(imp)).map(|_| ())
}

/// I5: the marker entry recording that the user APPROVED a prompt change for
/// `imp`'s agent.
///
/// Typed `Fact` (NOT `Lesson`) so `collect_lessons`'s Lesson filter ignores
/// it — it is a reference point for the effect check, not evidence. Tagged
/// `improvement-applied` + `agent:<name>` so `latest_applied_marker`
/// (improver.rs) can find it. The content names the agent and the approval
/// date, plus a short excerpt of the applied prompt: the store's fuzzy dedup
/// gate is token-based, so the date alone would not keep re-approvals of
/// different prompts distinct.
pub fn applied_marker(imp: &PendingImprovement) -> MemoryEntry {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prompt = imp
        .edited_prompt
        .as_deref()
        .or(imp.prompt_change.as_deref())
        .unwrap_or_default();
    let excerpt: String = prompt.chars().take(80).collect();
    let content = format!(
        "Prompt for agent '{}' changed via approved improvement on {} (applied prompt starts: '{}')",
        imp.agent_name, date, excerpt
    );
    let agent_tag = format!("agent:{}", imp.agent_name);
    MemoryEntry::new(
        MemoryType::Fact,
        &content,
        "improvement-review",
        &["improvement-applied", &agent_tag],
    )
}

/// I5: persist the approved prompt change of `imp` through the shared memory
/// store (same save path as F5's `remember_dismissal`). `Ok(())` on both
/// insert and duplicate; `Err` only on store failure.
pub fn remember_applied_prompt(
    memory: &MemoryManager,
    imp: &PendingImprovement,
) -> Result<(), String> {
    memory.add(applied_marker(imp)).map(|_| ())
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
