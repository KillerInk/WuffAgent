use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::agents::config::{AgentManager, WorkerConfig};
use crate::tools::ToolManager;
use super::theme::Theme;

/// UI dialog for adding/editing/removing agent configurations.
pub struct AgentConfigDialog {
    /// Loaded agents from disk.
    agents: Vec<WorkerConfig>,
    /// Currently selected agent index (-1 = none).
    selected_index: isize,
    /// Whether we're creating a new agent.
    is_new: bool,
    /// Form fields for the current agent being edited.
    name: String,
    description: String,
    system_prompt: String,
    priority: u32,
    max_concurrent: usize,
    enabled: bool,
    /// Checked status per tool index.
    tool_checkboxes: Vec<bool>,
    /// Available tool names from the tool registry.
    available_tools: Vec<String>,
    /// Derived from tool_checkboxes at save time.
    allowed_tools: Vec<String>,
    /// Error/success messages.
    message: Option<String>,
}

impl AgentConfigDialog {
    pub fn new(agent_manager: Arc<Mutex<AgentManager>>, tool_manager: &Arc<ToolManager>) -> Self {
        let agents = agent_manager
            .lock()
            .map(|m| m.list_agents().unwrap_or_default())
            .unwrap_or_default();
        let available_tools: Vec<String> = tool_manager
            .get_tool_definitions()
            .into_iter()
            .map(|t| t.function.name)
            .collect();
        let tool_checkboxes = vec![false; available_tools.len()];

        Self {
            agents,
            selected_index: -1,
            is_new: false,
            name: String::new(),
            description: String::new(),
            system_prompt: String::new(),
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            tool_checkboxes,
            available_tools,
            allowed_tools: Vec::new(),
            message: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, agent_manager: &Arc<Mutex<AgentManager>>) -> bool {
        let theme = Theme::from_name("dark");
        let mut closed = false;

        egui::Window::new("Agent Configuration")
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 540.0])
            .show(ctx, |ui| {
                ui.style_mut().spacing.item_spacing.y = 6.0;

                // Header
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Agent Configuration").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reload").clicked() {
                            if let Ok(m) = agent_manager.lock() {
                                self.agents = m.reload().unwrap_or_default();
                            }
                            self.clear_form();
                        }
                    });
                });
                ui.separator();

                // Split into left (agent list) and right (editor) panels
                egui::containers::ScrollArea::both()
                    .id_salt("agent_config_scroll")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Left panel: agent list
                            ui.allocate_ui_with_layout(
                                egui::Vec2::new(220.0, ui.available_height()),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    ui.heading("Agents");
                                    ui.separator();

                                    if ui.button("+ Add Agent").clicked() {
                                        self.start_new();
                                    }

                                    ui.separator();

                                    // Collect click actions to avoid borrowing self twice
                                    let mut action = None;
                                    for (i, agent) in self.agents.iter().enumerate() {
                                        let selected = i as isize == self.selected_index;
                                        let label = format!(
                                            "{} {}",
                                            if agent.enabled { "✓" } else { "○" },
                                            agent.name
                                        );
                                        let response = ui
                                            .add(egui::Button::new(label).fill(if selected {
                                                theme.selected_bg
                                            } else {
                                                ui.style().visuals.widgets.noninteractive.fg_stroke.color
                                            }))
                                            .on_hover_text(if agent.enabled {
                                                &agent.description
                                            } else {
                                                "Disabled"
                                            });
                                        if response.clicked() {
                                            action = Some((i, false));
                                            break;
                                        }
                                        if response.double_clicked() {
                                            action = Some((i, true));
                                            break;
                                        }
                                    }
                                    if let Some((i, double_clicked)) = action {
                                        if double_clicked {
                                            self.start_edit(self.agents[i].clone());
                                        } else {
                                            self.select_agent(i);
                                        }
                                    }
                                },
                            );

                            ui.separator();

                            // Right panel: editor
                            ui.allocate_ui_with_layout(
                                egui::Vec2::new(ui.available_width(), ui.available_height()),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    self.draw_editor(ui, agent_manager, &mut closed);
                                },
                            );
                        });
                    });
            });
        closed
    }

    fn draw_editor(
        &mut self,
        ui: &mut egui::Ui,
        agent_manager: &Arc<Mutex<AgentManager>>,
        closed: &mut bool,
    ) {
        let theme = Theme::from_name("dark");

        if self.is_new || self.selected_index >= 0 {
            // Form header
            ui.heading(if self.is_new { "New Agent" } else { "Edit Agent" });
            ui.separator();

            // Basic info
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Name:").size(12.0));
                    ui.text_edit_singleline(&mut self.name);
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Description:").size(12.0));
                    ui.text_edit_singleline(&mut self.description);
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Priority:").size(12.0));
                    ui.add(egui::Slider::new(&mut self.priority, 0..=100).text("Priority"));
                    ui.label(egui::RichText::new("(lower = selected first)").size(10.0));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Max Concurrent:").size(12.0));
                    ui.add(egui::Slider::new(&mut self.max_concurrent, 1..=16).text("Max concurrent"));
                });
                ui.checkbox(&mut self.enabled, "Enabled");
            });

            ui.separator();

            // System prompt
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("System Prompt:").size(12.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Clear").clicked() {
                            self.system_prompt.clear();
                        }
                    });
                });
                ui.add(
                    egui::TextEdit::multiline(&mut self.system_prompt)
                        .hint_text("Enter the system prompt / personality for this agent...")
                        .min_size(egui::Vec2::new(ui.available_width(), 120.0)),
                );
            });

            ui.separator();

            // Allowed tools
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Allowed Tools:").size(12.0).strong());
                    if ui.button("Select All").clicked() {
                        for cb in &mut self.tool_checkboxes {
                            *cb = true;
                        }
                    }
                    if ui.button("Clear All").clicked() {
                        for cb in &mut self.tool_checkboxes {
                            *cb = false;
                        }
                    }
                });
                for (i, tool) in self.available_tools.iter().enumerate() {
                    ui.checkbox(&mut self.tool_checkboxes[i], tool);
                }
            });

            ui.separator();

            // Message
            if let Some(ref msg) = self.message {
                let color = if msg.to_lowercase().contains("error") || msg.to_lowercase().contains("failed") {
                    theme.error
                } else {
                    theme.success
                };
                ui.label(egui::RichText::new(msg).color(color).size(11.0));
            }

            ui.separator();

            // Action buttons
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("Save").fill(theme.primary).rounding(6.0))
                    .clicked()
                {
                    if self.save(agent_manager) {
                        *closed = true;
                    }
                }
                if ui
                    .add(egui::Button::new("Cancel")
                        .fill(theme.surface_light)
                        .rounding(6.0))
                    .clicked()
                {
                    *closed = true;
                }
                if !self.is_new && self.selected_index >= 0 {
                    if ui
                        .add(egui::Button::new("Delete")
                            .fill(theme.error)
                            .rounding(6.0))
                        .clicked()
                    {
                        self.delete_agent(agent_manager);
                    }
                }
            });
        } else {
            // No agent selected — show info
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Select an agent from the list, or click \"+ Add Agent\" to create one.").strong());
            });
        }
    }

    fn save(&mut self, agent_manager: &Arc<Mutex<AgentManager>>) -> bool {
        if self.name.trim().is_empty() {
            self.message = Some("Name is required.".to_string());
            return false;
        }

        self.sync_tools_from_checkboxes();
        let config = WorkerConfig {
            name: self.name.trim().to_string(),
            description: self.description.trim().to_string(),
            system_prompt: self.system_prompt.clone(),
            allowed_tools: self.allowed_tools.clone(),
            priority: self.priority,
            max_concurrent: self.max_concurrent,
            enabled: self.enabled,
        };

        let result = if self.is_new {
            agent_manager.lock().map(|m| m.add_agent(&config))
        } else if let Some(idx) = self.selected_index() {
            let old_name = self.agents[idx].name.clone();
            agent_manager.lock().map(|m| m.edit_agent(&old_name, &config))
        } else {
            return false;
        };

        match result {
            Ok(Ok(())) => {
                self.message = Some("Agent saved successfully.".to_string());
                if let Ok(m) = agent_manager.lock() {
                    self.agents = m.reload().unwrap_or_default();
                }
                self.clear_form();
                true
            }
            Ok(Err(e)) => {
                self.message = Some(format!("Error: {}", e));
                false
            }
            Err(e) => {
                self.message = Some(format!("Lock error: {}", e));
                false
            }
        }
    }

    fn delete_agent(&mut self, agent_manager: &Arc<Mutex<AgentManager>>) {
        if let Some(idx) = self.selected_index() {
            let name = self.agents[idx].name.clone();
            if let Ok(m) = agent_manager.lock() {
                match m.remove_agent(&name) {
                    Ok(()) => {
                        self.agents.remove(idx);
                        self.clear_form();
                    }
                    Err(e) => {
                        self.message = Some(format!("Error: {}", e));
                    }
                }
            }
        }
    }

    fn select_agent(&mut self, idx: usize) {
        self.selected_index = idx as isize;
        self.is_new = false;
        // Clone needed fields before dropping the borrow
        let agent_name = self.agents[idx].name.clone();
        let agent_desc = self.agents[idx].description.clone();
        let agent_prompt = self.agents[idx].system_prompt.clone();
        let agent_priority = self.agents[idx].priority;
        let agent_max_concurrent = self.agents[idx].max_concurrent;
        let agent_enabled = self.agents[idx].enabled;
        let agent_allowed = self.agents[idx].allowed_tools.clone();

        self.name = agent_name;
        self.description = agent_desc;
        self.system_prompt = agent_prompt;
        self.priority = agent_priority;
        self.max_concurrent = agent_max_concurrent;
        self.enabled = agent_enabled;

        self.sync_tools_from_agent(&agent_allowed);
        self.message = None;
    }

    fn start_new(&mut self) {
        self.is_new = true;
        self.selected_index = -1;
        self.clear_form();
    }

    fn start_edit(&mut self, agent: WorkerConfig) {
        self.is_new = false;
        if let Some(idx) = self.agents.iter().position(|a| a.name == agent.name) {
            self.select_agent(idx);
        }
    }

    fn clear_form(&mut self) {
        self.is_new = false;
        self.selected_index = -1;
        self.name.clear();
        self.description.clear();
        self.system_prompt.clear();
        self.priority = 0;
        self.max_concurrent = 1;
        self.enabled = true;
        self.allowed_tools.clear();
        for cb in &mut self.tool_checkboxes {
            *cb = false;
        }
        self.message = None;
    }

    fn selected_index(&self) -> Option<usize> {
        if self.selected_index >= 0 {
            Some(self.selected_index as usize)
        } else {
            None
        }
    }

    fn sync_tools_from_agent(&mut self, allowed: &Vec<String>) {
        self.allowed_tools = allowed.clone();
        // Reset checkboxes to match
        self.tool_checkboxes = self
            .available_tools
            .iter()
            .map(|t| allowed.contains(t))
            .collect();
    }

    fn sync_tools_from_checkboxes(&mut self) {
        self.allowed_tools = self
            .available_tools
            .iter()
            .zip(&self.tool_checkboxes)
            .filter_map(|(name, checked)| if *checked { Some(name.clone()) } else { None })
            .collect();
    }
}
