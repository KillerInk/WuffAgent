use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agents::config::{AgentConfig, AgentManager};
use crate::tools::ToolManager;
use super::agent_history;
use super::theme::Theme;

/// UI dialog for adding/editing/removing agent configurations.
pub struct AgentConfigDialog {
    /// Loaded agents from disk.
    agents: Vec<AgentConfig>,
    /// Currently selected agent index (-1 = none).
    selected_index: isize,
    /// Whether we're creating a new agent.
    is_new: bool,
    /// Form fields for the current agent being edited.
    name: String,
    description: String,
    system_prompt: String,
    enabled: bool,
    /// Reasoning effort for this agent (Off = inherit the global toggle).
    reasoning_effort: crate::types::ReasoningEffort,
    /// Shell settings (persisted as the agent's `shell_config`).
    shell_enabled: bool,
    shell_type: String,
    shell_timeout_ms: u32,
    /// Comma-separated list of allowed command patterns (empty = allow all
    /// non-dangerous commands).
    shell_allowed_commands: String,
    /// Whether this agent may hand off the session to another agent.
    handoff_enabled: bool,
    /// Comma-separated handoff target names (empty = any enabled agent).
    handoff_targets: String,
    /// Checked status per tool index.
    tool_checkboxes: Vec<bool>,
    /// Available tool names from the tool registry.
    available_tools: Vec<String>,
    /// Derived from tool_checkboxes at save time.
    allowed_tools: Vec<String>,
    /// Error/success messages.
    message: Option<String>,
    /// Whether the window is open (drives the title-bar close button).
    open: bool,
}

impl AgentConfigDialog {
    pub fn new(agent_manager: Arc<Mutex<AgentManager>>, tool_manager: &Arc<ToolManager>) -> Self {
        let agents = agent_manager
            .lock()
            .map(|m| m.list_agents().unwrap_or_default())
            .unwrap_or_default();
        // `shell` is intentionally not in the tools list: its availability is
        // controlled solely by the "Enable shell tool" checkbox (a disabled
        // shell is removed from the agent's schema entirely).
        let available_tools: Vec<String> = tool_manager
            .get_tool_definitions()
            .into_iter()
            .map(|t| t.function.name)
            .filter(|name| name != "shell")
            .collect();
        let tool_checkboxes = vec![false; available_tools.len()];

        Self {
            agents,
            selected_index: -1,
            is_new: false,
            name: String::new(),
            description: String::new(),
            system_prompt: String::new(),
            enabled: true,
            reasoning_effort: crate::types::ReasoningEffort::default(),
            shell_enabled: false,
            shell_type: "powershell".to_string(),
            shell_timeout_ms: 300_000,
            shell_allowed_commands: String::new(),
            handoff_enabled: false,
            handoff_targets: String::new(),
            tool_checkboxes,
            available_tools,
            allowed_tools: Vec::new(),
            message: None,
            open: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, agent_manager: &Arc<Mutex<AgentManager>>) -> bool {
        let theme = Theme::from_name("dark");
        // Copy the flag out so the title-bar close button can toggle it
        // without clashing with the closure's `&mut self` borrow below.
        let mut open = self.open;

        egui::Window::new("Agent Configuration")
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 540.0])
            .open(&mut open)
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

                                    if self.agents.is_empty() {
                                        ui.label("No agents configured.");
                                    } else {
                                        // Pre-collect agent data to avoid borrowing self mutably inside the loop.
                                        // (Display data + index only: selecting an agent copies
                                        // fields via `select_agent`, which reads `self.agents`.)
                                        struct AgentButtonData {
                                            label: String,
                                            bg: egui::Color32,
                                            idx: usize,
                                        }
                                        let button_data: Vec<AgentButtonData> = self.agents.iter().enumerate().map(|(i, agent)| {
                                            let selected = i as isize == self.selected_index;
                                            AgentButtonData {
                                                label: format!("[{}] {}", if agent.enabled { "x" } else { " " }, agent.name),
                                                bg: if selected { egui::Color32::from_rgb(0x33, 0x66, 0xCC) } else { egui::Color32::from_rgb(0x33, 0x33, 0x33) },
                                                idx: i,
                                            }
                                        }).collect();
                                        for bd in &button_data {
                                            if ui.add(egui::Button::new(&bd.label).fill(bd.bg)).clicked() {
                                                self.select_agent(bd.idx);
                                            }
                                        }
                                    }

                                    if !self.agents.is_empty()
                                        && self.selected_index >= 0
                                    {
                                        ui.separator();
                                        if ui
                                            .add_enabled(
                                                true,
                                                egui::Button::new("Delete")
                                                    .fill(egui::Color32::from_rgb(0xCC, 0x33, 0x33)),
                                            )
                                            .clicked()
                                        {
                                            self.delete_agent(agent_manager);
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
                                    ui.heading("Agent Editor");
                                    ui.separator();

                                    if self.is_new || self.selected_index >= 0 {
                                        // F4: prompt history of the selected EXISTING agent —
                                        // snapshots from every directory the manager can see
                                        // (primary + search dirs; a profile's snapshots live
                                        // next to its file, see F3).
                                        let history_agent: Option<String> = if !self.is_new {
                                            self.selected_index()
                                                .filter(|&idx| idx < self.agents.len())
                                                .map(|idx| self.agents[idx].name.clone())
                                        } else {
                                            None
                                        };
                                        let history_entries: Option<Vec<agent_history::HistoryEntry>> =
                                            history_agent.as_ref().and_then(|name| {
                                                agent_manager.lock().ok().map(|m| {
                                                    let mut dirs: Vec<PathBuf> = vec![m.agents_dir().clone()];
                                                    for d in m.search_dirs() {
                                                        if !dirs.contains(d) {
                                                            dirs.push(d.clone());
                                                        }
                                                    }
                                                    agent_history::list_history(&dirs, name)
                                                })
                                            });
                                        let mut revert_target: Option<agent_history::HistoryEntry> = None;

                                        // Agent fields
                                        ui.vertical(|ui| {
                                            ui.label("Name:");
                                            ui.text_edit_singleline(&mut self.name);

                                            ui.label("Description:");
                                            ui.text_edit_singleline(&mut self.description);

                                            ui.label("System Prompt:");
                                            ui.text_edit_multiline(&mut self.system_prompt);

                                            ui.horizontal(|ui| {
                                                ui.checkbox(&mut self.enabled, "Enabled");
                                                ui.separator();
                                                ui.label("Reasoning effort:");
                                                for variant in crate::types::ReasoningEffort::VARIANTS {
                                                    ui.selectable_value(&mut self.reasoning_effort, variant, variant.name());
                                                }
                                            });

                                            ui.separator();
                                            ui.group(|ui| {
                                                ui.checkbox(&mut self.shell_enabled, "Enable shell tool");
                                                ui.horizontal(|ui| {
                                                    ui.label("Shell type:");
                                                    for t in ["powershell", "cmd", "bash"] {
                                                        ui.selectable_value(
                                                            &mut self.shell_type,
                                                            t.to_string(),
                                                            t,
                                                        );
                                                    }
                                                });
                                                ui.horizontal(|ui| {
                                                    ui.label("Timeout (ms):");
                                                    ui.add(
                                                        egui::DragValue::new(&mut self.shell_timeout_ms)
                                                            .range(1000..=600_000),
                                                    );
                                                });
                                                ui.label("Allowed commands (comma-separated patterns; empty = allow all non-dangerous):");
                                                ui.text_edit_singleline(&mut self.shell_allowed_commands);
                                            });

                                            ui.separator();
                                            ui.group(|ui| {
                                                ui.checkbox(
                                                    &mut self.handoff_enabled,
                                                    "Allow agent handoff (this agent may switch the session to another agent)",
                                                );
                                                ui.label("Handoff targets (comma-separated agent names; empty = any enabled agent):");
                                                ui.text_edit_singleline(&mut self.handoff_targets);
                                            });

                                            ui.separator();
                                            ui.label("Allowed Tools:");

                                            // Tool checkboxes
                                            for (i, tool) in self.available_tools.iter().enumerate() {
                                                ui.checkbox(&mut self.tool_checkboxes[i], tool);
                                            }

                                            // F4: prompt history + per-version Revert.
                                            if history_agent.is_some() {
                                                ui.separator();
                                                ui.group(|ui| {
                                                    ui.label(egui::RichText::new("Prompt history (newest first):").strong());
                                                    ui.label(
                                                        egui::RichText::new(
                                                            "A snapshot is saved before every edit. Revert restores the selected version; the current state is snapshotted first, so a revert is itself reversible.",
                                                        )
                                                        .weak(),
                                                    );
                                                    match &history_entries {
                                                        None => {
                                                            ui.label(
                                                                egui::RichText::new("(could not read prompt history)")
                                                                    .weak(),
                                                            );
                                                        }
                                                        Some(entries) if entries.is_empty() => {
                                                            ui.label(
                                                                egui::RichText::new("No prompt history yet.")
                                                                    .weak(),
                                                            );
                                                        }
                                                        Some(entries) => {
                                                            for e in entries.iter() {
                                                                ui.horizontal(|ui| {
                                                                    let mut ts_label = agent_history::format_ts(e.ts);
                                                                    if e.seq > 0 {
                                                                        ts_label.push_str(&format!("#{}", e.seq));
                                                                    }
                                                                    ui.label(ts_label);
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            agent_history::prompt_preview(&e.path),
                                                                        )
                                                                        .weak()
                                                                        .monospace(),
                                                                    );
                                                                    if ui.small_button("Revert").clicked() {
                                                                        revert_target = Some(e.clone());
                                                                    }
                                                                });
                                                            }
                                                        }
                                                    }
                                                });
                                            }

                                            ui.separator();
                                            ui.horizontal(|ui| {
                                                if ui.add(
                                                    egui::Button::new("Save")
                                                        .fill(theme.success),
                                                )
                                                .clicked()
                                                {
                                                    if self.save(agent_manager) {
                                                        self.clear_form();
                                                    }
                                                }
                                            });

                                            if let Some(msg) = &self.message {
                                                if msg.contains("Error") {
                                                    ui.label(egui::RichText::new(msg).color(egui::Color32::from_rgb(0xCC, 0x33, 0x33)));
                                                } else {
                                                    ui.label(egui::RichText::new(msg).color(theme.success));
                                                }
                                                self.message = None;
                                            }
                                        });

                                        // F4: execute a requested revert (file I/O + list
                                        // refresh), outside the drawing closure.
                                        if let (Some(name), Some(entry)) = (&history_agent, revert_target) {
                                            match agent_history::revert(&entry.dir, name, &entry) {
                                                Ok(config) => {
                                                    if let Ok(m) = agent_manager.lock() {
                                                        self.agents = m.reload().unwrap_or_default();
                                                    }
                                                    if let Some(pos) =
                                                        self.agents.iter().position(|a| a.name == config.name)
                                                    {
                                                        self.selected_index = pos as isize;
                                                    }
                                                    self.load_into_form(&config);
                                                    self.message = Some(format!(
                                                        "Reverted '{}' to {}.",
                                                        config.name,
                                                        agent_history::format_ts(entry.ts)
                                                    ));
                                                }
                                                Err(e) => {
                                                    self.message = Some(format!(
                                                        "Error: revert of '{}' failed: {}",
                                                        name, e
                                                    ));
                                                }
                                            }
                                        }
                                    } else {
                                        // No agent selected Ã¢â‚¬â€ show info
                                        ui.vertical_centered(|ui| {
                                            ui.label(egui::RichText::new("Select an agent from the list, or click \"+ Add Agent\" to create one.").strong());
                                        });
                                    }
                                },
                            );
                        });
                    });
            });
        self.open = open;
        !open
    }

    fn save(&mut self, agent_manager: &Arc<Mutex<AgentManager>>) -> bool {
        if self.name.trim().is_empty() {
            self.message = Some("Name is required.".to_string());
            return false;
        }

        self.sync_tools_from_checkboxes();
        let config = AgentConfig {
            name: self.name.trim().to_string(),
            description: self.description.trim().to_string(),
            system_prompt: self.system_prompt.clone(),
            allowed_tools: self.allowed_tools.clone(),
            enabled: self.enabled,
            task_timeout_ms: 60_000,
            shell_config: wuffagent_core::agents::config::ShellConfig {
                allowed_commands: self
                    .shell_allowed_commands
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                shell_type: self.shell_type.clone(),
                shell_timeout_ms: self.shell_timeout_ms as u64,
                shell_enabled: self.shell_enabled,
                working_dir: None,
            },
            agents_dir: std::path::PathBuf::from(""),
            agents_search_dirs: Vec::new(),
            custom_prompts: std::collections::HashMap::new(),
            reasoning_effort: self.reasoning_effort,
            trim_config: wuffagent_core::trimming::config::TrimConfig::default(),
            handoff_enabled: self.handoff_enabled,
            handoff_targets: self
                .handoff_targets
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            // The agent editor has no restart toggle yet; keep the default
            // (enabled) so created/edited profiles can restart WuffAgent.
            restart_enabled: true,
        };

        // Ensure the directory exists before saving
        if let Ok(m) = agent_manager.lock() {
            if let Err(e) = std::fs::create_dir_all(m.agents_dir()) {
                self.message = Some(format!("Failed to create directory: {}", e));
                return false;
            }
        }

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
        if idx < self.agents.len() {
            // Clone out first: `load_into_form` takes `&mut self`, so we
            // cannot pass `&self.agents[idx]` directly.
            let agent = self.agents[idx].clone();
            self.load_into_form(&agent);
        }
    }

    /// Load an externally-sourced config into the form fields (e.g. the one
    /// `revert_agent` returns after a F4 revert restored a historical
    /// version).
    fn load_into_form(&mut self, agent: &AgentConfig) {
        self.is_new = false;
        self.name = agent.name.clone();
        self.description = agent.description.clone();
        self.system_prompt = agent.system_prompt.clone();
        self.enabled = agent.enabled;
        self.reasoning_effort = agent.reasoning_effort;
        self.shell_enabled = agent.shell_config.shell_enabled;
        self.shell_type = agent.shell_config.shell_type.clone();
        self.shell_timeout_ms = agent.shell_config.shell_timeout_ms as u32;
        self.shell_allowed_commands = agent.shell_config.allowed_commands.join(", ");
        self.handoff_enabled = agent.handoff_enabled;
        self.handoff_targets = agent.handoff_targets.join(", ");
        self.sync_tools_from_agent(&agent.allowed_tools);
        self.message = None;
    }

    fn start_new(&mut self) {
        // Clear first: clear_form() resets is_new to false, so it must be
        // set afterwards or the editor panel never appears.
        self.clear_form();
        self.is_new = true;
    }

    fn clear_form(&mut self) {
        self.is_new = false;
        self.selected_index = -1;
        self.name.clear();
        self.description.clear();
        self.system_prompt.clear();
        self.enabled = true;
        self.reasoning_effort = crate::types::ReasoningEffort::default();
        self.shell_enabled = false;
        self.shell_type = "powershell".to_string();
        self.shell_timeout_ms = 300_000;
        self.shell_allowed_commands.clear();
        self.handoff_enabled = false;
        self.handoff_targets.clear();
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

    fn sync_tools_from_agent(&mut self, allowed: &[String]) {
        self.allowed_tools = allowed.to_vec();
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
