//! F6: the right panel — the agent editor form (basic fields, shell group,
//! handoff group) plus the F4 prompt-history revert (draw + post-draw
//! execution) and the save row.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::AgentConfigDialog;
use crate::ui::agent_history;
use crate::ui::theme::Theme;
use wuffagent_core::agents::config::AgentManager;

impl AgentConfigDialog {
    /// Right panel: the form for the selected (or new) agent, plus the
    /// post-draw prompt-history revert execution (F4).
    pub(super) fn draw_editor(
        &mut self,
        ui: &mut egui::Ui,
        agent_manager: &Arc<Mutex<AgentManager>>,
        theme: &Theme,
    ) {
        ui.heading("Agent Editor");
        ui.separator();

        if self.is_new || self.selected_index >= 0 {
            // F4: prompt history of the selected EXISTING agent - snapshots
            // from every directory the manager can see (primary + search
            // dirs; a profile's snapshots live next to its file, see F3).
            let history_agent: Option<String> = if self.is_new {
                None
            } else {
                self.selected_index()
                    .filter(|&idx| idx < self.agents.len())
                    .map(|idx| self.agents[idx].name.clone())
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
            ui.vertical(|ui| {
                self.draw_basic_fields(ui);

                ui.separator();
                self.draw_shell_group(ui);

                ui.separator();
                self.draw_handoff_group(ui);

                ui.separator();
                self.draw_tools_and_metrics(ui, history_agent.as_deref());

                // 2d: the golden/regression eval panel (existing agents only).
                if let Some(name) = history_agent.as_deref() {
                    ui.separator();
                    self.draw_evals(ui, name);
                }

                revert_target =
                    self.draw_prompt_history(ui, &history_entries, history_agent.as_deref());

                ui.separator();
                self.draw_save_row(ui, agent_manager, theme);
            });

            // F4: execute a requested revert (file I/O + list refresh),
            // outside the drawing closure.
            if let (Some(name), Some(entry)) = (&history_agent, revert_target) {
                self.execute_revert(agent_manager, name, &entry);
            }
        } else {
            // No agent selected - show info
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(
                        "Select an agent from the list, or click \"+ Add Agent\" to create one.",
                    )
                    .strong(),
                );
            });
        }
    }

    /// Name / description / system prompt / enabled / reasoning effort.
    fn draw_basic_fields(&mut self, ui: &mut egui::Ui) {
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
            for variant in wuffagent_core::types::ReasoningEffort::VARIANTS {
                ui.selectable_value(&mut self.reasoning_effort, variant, variant.name());
            }
        });
    }

    /// The shell-tool settings group.
    fn draw_shell_group(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.checkbox(&mut self.shell_enabled, "Enable shell tool");
            ui.horizontal(|ui| {
                ui.label("Shell type:");
                for t in ["powershell", "cmd", "bash"] {
                    ui.selectable_value(&mut self.shell_type, t.to_string(), t);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Timeout (ms):");
                ui.add(egui::DragValue::new(&mut self.shell_timeout_ms).range(1000..=600_000));
            });
            ui.label(
                "Allowed commands (comma-separated patterns; empty = allow all non-dangerous):",
            );
            ui.text_edit_singleline(&mut self.shell_allowed_commands);
        });
    }

    /// The handoff settings group.
    fn draw_handoff_group(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.checkbox(
                &mut self.handoff_enabled,
                "Allow agent handoff (this agent may switch the session to another agent)",
            );
            ui.label("Handoff targets (comma-separated agent names; empty = any enabled agent):");
            ui.text_edit_singleline(&mut self.handoff_targets);
        });
    }

    /// F4: prompt history + per-version Revert. Returns the entry the user
    /// asked to revert (executed by the caller, outside the drawing closure).
    fn draw_prompt_history(
        &mut self,
        ui: &mut egui::Ui,
        history_entries: &Option<Vec<agent_history::HistoryEntry>>,
        history_agent: Option<&str>,
    ) -> Option<agent_history::HistoryEntry> {
        if history_agent.is_none() {
            return None;
        }
        let mut revert_target: Option<agent_history::HistoryEntry> = None;
        ui.separator();
        ui.group(|ui| {
            ui.label(egui::RichText::new("Prompt history (newest first):").strong());
            ui.label(
                egui::RichText::new(
                    "A snapshot is saved before every edit. Revert restores the selected version; the current state is snapshotted first, so a revert is itself reversible.",
                )
                .weak(),
            );
            match history_entries {
                None => {
                    ui.label(
                        egui::RichText::new("(could not read prompt history)").weak(),
                    );
                }
                Some(entries) if entries.is_empty() => {
                    ui.label(egui::RichText::new("No prompt history yet.").weak());
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
                                egui::RichText::new(agent_history::prompt_preview(&e.path))
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
        revert_target
    }

    /// Save button + the one-shot status message.
    fn draw_save_row(
        &mut self,
        ui: &mut egui::Ui,
        agent_manager: &Arc<Mutex<AgentManager>>,
        theme: &Theme,
    ) {
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new("Save").fill(theme.success))
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
    }

    /// F4: run the requested prompt-history revert (file I/O + list refresh).
    fn execute_revert(
        &mut self,
        agent_manager: &Arc<Mutex<AgentManager>>,
        name: &str,
        entry: &agent_history::HistoryEntry,
    ) {
        match agent_history::revert(&entry.dir, name, entry) {
            Ok(config) => {
                if let Ok(m) = agent_manager.lock() {
                    self.agents = m.reload().unwrap_or_default();
                }
                if let Some(pos) = self.agents.iter().position(|a| a.name == config.name) {
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
                self.message = Some(format!("Error: revert of '{}' failed: {}", name, e));
            }
        }
    }
}
