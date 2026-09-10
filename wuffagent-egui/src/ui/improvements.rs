use eframe::egui;
use tracing;

use super::state::ChatApp;
use super::theme::Theme;

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
}

impl ImprovementsPanel {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            show_panel: false,
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
    pub fn draw(&mut self, ctx: &egui::Context) {
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

                if self.pending.is_empty() {
                    ui.label("No pending improvements yet.");
                    if ui.button("Close").clicked() {
                        self.show_panel = false;
                    }
                    return;
                }

                // Collect indices to remove to avoid borrow checker issues
                let mut to_remove = Vec::new();
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
                                egui::Button::new("âœ“ Approve").fill(theme.primary)
                            ).clicked() {
                                tracing::info!("Approved improvement for agent {}", imp.agent_name);
                            }
                            if ui.add(egui::Button::new("âœ— Dismiss")).clicked() {
                                to_remove.push(i);
                            }
                        });
                        ui.separator();
                    });
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

impl ChatApp {
    /// Draw the improvements panel.
    pub(super) fn draw_improvements_panel(&mut self, ctx: &egui::Context) {
        self.improvements_panel.draw(ctx);
    }
}
