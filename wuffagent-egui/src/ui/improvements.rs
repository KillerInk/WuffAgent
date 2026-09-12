use std::path::PathBuf;

use eframe::egui;
use tracing;

use super::state::ChatApp;
use super::theme::Theme;
use crate::agents::config::{AgentConfig, AgentManager};

/// State for a single pending improvement suggestion.
#[derive(Clone)]
pub struct PendingImprovement {
    pub agent_name: String,
    pub prompt_change: Option<String>,
    pub rationale: String,
    pub new_agents: Vec<wuffagent_core::memory::NewAgentProposal>,
}

impl From<wuffagent_core::memory::ImprovementSuggestion> for PendingImprovement {
    fn from(s: wuffagent_core::memory::ImprovementSuggestion) -> Self {
        Self {
            agent_name: s.agent_name,
            prompt_change: s.prompt_change,
            rationale: s.rationale,
            new_agents: s.new_agents,
        }
    }
}

/// Panel for showing pending agent improvement suggestions.
pub struct ImprovementsPanel {
    pub pending: Vec<PendingImprovement>,
    pub show_panel: bool,
    /// Last approve/persist outcome, shown to the user in the panel.
    message: Option<String>,
}

impl ImprovementsPanel {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            show_panel: false,
            message: None,
        }
    }

    /// Handle an improvement suggested event.
    pub fn handle_improvement_suggested(
        &mut self,
        agent_name: &str,
        suggestions: Vec<wuffagent_core::memory::ImprovementSuggestion>,
    ) {
        if suggestions.is_empty() {
            return;
        }
        self.pending.clear();
        for s in suggestions {
            self.pending.push(s.into());
        }
        self.show_panel = true;
        tracing::info!(
            "Received {} improvement suggestion(s) for agent '{}'",
            self.pending.len(),
            agent_name
        );
    }

    /// Draw the improvements panel.
    pub fn draw(&mut self, ctx: &egui::Context, agent_manager: &AgentManager) {
        if !self.show_panel {
            return;
        }

        egui::Window::new("Agent Improvements")
            .collapsible(true)
            .resizable(true)
            .default_size([500.0, 400.0])
            .show(ctx, |ui| {
                let theme = Theme::from_name("dark");
                ui.visuals_mut().panel_fill = theme.surface;

                ui.heading("Improvement Suggestions");
                ui.separator();

                if let Some(msg) = &self.message {
                    ui.add(egui::Label::new(msg.as_str()).wrap());
                    ui.separator();
                }

                if self.pending.is_empty() {
                    ui.label("No pending improvements yet.");
                    if ui.button("Close").clicked() {
                        self.show_panel = false;
                    }
                    return;
                }

                // Collect indices to remove / approve to avoid borrow checker issues
                let mut to_remove = Vec::new();
                let mut to_approve = Vec::new();
                for (i, imp) in self.pending.iter().enumerate() {
                    ui.collapsing(format!("Agent: {}", imp.agent_name), |ui| {
                        ui.label(format!("Rationale: {}", imp.rationale));
                        ui.separator();
                        if let Some(ref prompt) = imp.prompt_change {
                            ui.label("Proposed prompt change:");
                            let mut prompt_edit = prompt.clone();
                            ui.add(egui::TextEdit::multiline(&mut prompt_edit)
                                .desired_rows(6)
                                .code_editor()
                                .desired_width(f32::INFINITY));
                        } else {
                            ui.label("No prompt change suggested");
                        }
                        if !imp.new_agents.is_empty() {
                            ui.separator();
                            ui.label("New agent proposals:");
                            for (j, na) in imp.new_agents.iter().enumerate() {
                                ui.collapsing(format!("Agent #{}", j + 1), |ui| {
                                    ui.label(format!("Name: {}", na.name));
                                    ui.label(format!("Description: {}", na.description));
                                    ui.label("System prompt:");
                                    let mut sp = na.system_prompt.clone();
                                    ui.add(egui::TextEdit::multiline(&mut sp)
                                        .desired_rows(4)
                                        .code_editor()
                                        .desired_width(f32::INFINITY));
                                    if !na.allowed_tools.is_empty() {
                                        ui.label(format!("Tools: {}", na.allowed_tools.join(", ")));
                                    }
                                });
                            }
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.add_enabled(imp.prompt_change.is_some(),
                                egui::Button::new("✓ Approve").fill(theme.primary)
                            ).clicked() {
                                to_approve.push(i);
                            }
                            if ui.add(egui::Button::new("✗ Dismiss")).clicked() {
                                to_remove.push(i);
                            }
                        });
                        ui.separator();
                    });
                }
                // Apply approved improvements (persisting to the agents dir), then
                // remove them alongside any dismissed entries.
                for i in to_approve.into_iter().rev() {
                    let outcome = apply_improvement(agent_manager, &self.pending[i]);
                    self.message = Some(outcome);
                    to_remove.push(i);
                }
                // Remove in reverse order to preserve indices
                for i in to_remove.into_iter().rev() {
                    self.pending.remove(i);
                }
                if self.pending.is_empty() {
                    self.show_panel = false;
                }

                if ui.button("Close").clicked() {
                    self.show_panel = false;
                }
            });
    }
}

/// Persist an approved improvement: update the target agent's system prompt
/// (when a prompt change is present) and create any proposed new agents.
/// Returns a human-readable summary of what was written (or the error).
pub fn apply_improvement(
    agent_manager: &AgentManager,
    imp: &PendingImprovement,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if let Some(new_prompt) = imp.prompt_change.as_deref() {
        if new_prompt.trim().is_empty() {
            errors.push(format!("agent '{}': empty prompt change", imp.agent_name));
        } else {
            match agent_manager.get_agent(&imp.agent_name) {
                Some(mut config) => {
                    config.system_prompt = new_prompt.to_string();
                    match agent_manager.edit_agent(&imp.agent_name, &config) {
                        Ok(()) => parts.push(format!("updated prompt for '{}'", imp.agent_name)),
                        Err(e) => errors.push(format!("failed to update '{}': {}", imp.agent_name, e)),
                    }
                }
                None => errors.push(format!("agent '{}' not found", imp.agent_name)),
            }
        }
    }

    for na in &imp.new_agents {
        if na.name.trim().is_empty() {
            errors.push("new agent has an empty name".to_string());
            continue;
        }
        let config = AgentConfig {
            name: na.name.clone(),
            description: na.description.clone(),
            system_prompt: na.system_prompt.clone(),
            allowed_tools: na.allowed_tools.clone(),
            ..Default::default()
        };
        match agent_manager.add_agent(&config) {
            Ok(()) => parts.push(format!("created new agent '{}'", na.name)),
            Err(e) => errors.push(format!("failed to create '{}': {}", na.name, e)),
        }
    }

    if parts.is_empty() && errors.is_empty() {
        return format!("approved (no changes for '{}')", imp.agent_name);
    }
    let mut msg = if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join("; "))
    };
    if !errors.is_empty() {
        msg.push_str(&format!("[errors] {}", errors.join("; ")));
    }
    msg
}

impl ChatApp {
    /// Build an [`AgentManager`] mirroring the discovery dirs used elsewhere
    /// (config-dir agents as primary, then cwd/exe-dir `agents/`).
    pub(super) fn build_agent_manager(&self) -> AgentManager {
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| PathBuf::from("agents"));
        let mut manager = AgentManager::new(agents_dir);
        if let Ok(cwd) = std::env::current_dir() {
            manager.add_search_dir(cwd.join("agents"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                manager.add_search_dir(exe_dir.join("agents"));
            }
        }
        manager
    }

    /// Draw the improvements panel.
    pub(super) fn draw_improvements_panel(&mut self, ctx: &egui::Context) {
        let agent_manager = self.build_agent_manager();
        self.improvements_panel.draw(ctx, &agent_manager);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_agents_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wuffagent-egui-imp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn existing_agent(name: &str) -> AgentConfig {
        AgentConfig {
            name: name.to_string(),
            description: "existing".to_string(),
            system_prompt: "old prompt".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_apply_improvement_updates_prompt_and_creates_agent() {
        let dir = temp_agents_dir("update");
        let manager = AgentManager::new(dir.clone());
        manager.add_agent(&existing_agent("coder")).unwrap();

        let imp = PendingImprovement {
            agent_name: "coder".to_string(),
            prompt_change: Some("new prompt".to_string()),
            rationale: "test".to_string(),
            new_agents: vec![wuffagent_core::memory::NewAgentProposal {
                name: "helper".to_string(),
                description: "a helper".to_string(),
                system_prompt: "helper prompt".to_string(),
                allowed_tools: vec!["file_io".to_string()],
            }],
        };

        let msg = apply_improvement(&manager, &imp);
        assert!(msg.contains("updated prompt for 'coder'"), "msg: {}", msg);
        assert!(msg.contains("created new agent 'helper'"), "msg: {}", msg);

        // Prompt change persisted.
        let coder = manager.get_agent("coder").expect("coder agent");
        assert_eq!(coder.system_prompt, "new prompt");
        // New agent file written and parseable.
        let helper = manager.get_agent("helper").expect("helper agent");
        assert_eq!(helper.system_prompt, "helper prompt");
        assert_eq!(helper.allowed_tools, vec!["file_io"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_apply_improvement_missing_agent_reports_error() {
        let dir = temp_agents_dir("missing");
        let manager = AgentManager::new(dir.clone());

        let imp = PendingImprovement {
            agent_name: "ghost".to_string(),
            prompt_change: Some("p".to_string()),
            rationale: "test".to_string(),
            new_agents: vec![],
        };

        let msg = apply_improvement(&manager, &imp);
        assert!(msg.contains("agent 'ghost' not found"), "msg: {}", msg);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
