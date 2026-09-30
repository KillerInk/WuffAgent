//! F6: the left panel — the agent list with Add/Delete buttons.

use eframe::egui;
use std::sync::{Arc, Mutex};

use super::AgentConfigDialog;
use wuffagent_core::agents::config::AgentManager;

impl AgentConfigDialog {
    /// Left panel: the agent list with Add/Delete buttons.
    pub(super) fn draw_agent_list(
        &mut self,
        ui: &mut egui::Ui,
        agent_manager: &Arc<Mutex<AgentManager>>,
    ) {
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
            // (Display data + index only: selecting an agent copies fields via
            // `select_agent`, which reads `self.agents`.)
            struct AgentButtonData {
                label: String,
                bg: egui::Color32,
                idx: usize,
            }
            let button_data: Vec<AgentButtonData> = self
                .agents
                .iter()
                .enumerate()
                .map(|(i, agent)| {
                    let selected = i as isize == self.selected_index;
                    AgentButtonData {
                        label: format!(
                            "[{}] {}",
                            if agent.enabled { "x" } else { " " },
                            agent.name
                        ),
                        bg: if selected {
                            egui::Color32::from_rgb(0x33, 0x66, 0xCC)
                        } else {
                            egui::Color32::from_rgb(0x33, 0x33, 0x33)
                        },
                        idx: i,
                    }
                })
                .collect();
            for bd in &button_data {
                if ui.add(egui::Button::new(&bd.label).fill(bd.bg)).clicked() {
                    self.select_agent(bd.idx);
                }
            }
        }

        if !self.agents.is_empty() && self.selected_index >= 0 {
            ui.separator();
            if ui
                .add_enabled(
                    true,
                    egui::Button::new("Delete").fill(egui::Color32::from_rgb(0xCC, 0x33, 0x33)),
                )
                .clicked()
            {
                self.delete_agent(agent_manager);
            }
        }
    }

    /// Remove the selected agent from disk + the in-memory list.
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
}
