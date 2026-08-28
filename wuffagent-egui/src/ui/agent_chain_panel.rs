use eframe::egui;

use super::state::ChatApp;
use crate::sessions::model::AgentChainEntry;

impl ChatApp {
    /// Show the agent chain side panel on the right.
    pub(super) fn draw_agent_chain_panel(&mut self, ui: &mut egui::Ui) {
        let state = &self.agent_chain_state;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
            ui.label(egui::RichText::new("Agent Chain").strong());
            if ui.small_button("✕").clicked() {
                self.show_agent_chain = false;
            }
        });

        ui.separator();

        if state.entries.is_empty() && state.current_agent.is_none() && !state.cancelled {
            ui.label(egui::RichText::new("No active agent chain").italics().size(11.0));
            return;
        }

        // Show active agent indicator
        if let Some(ref agent) = state.current_agent {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(format!("Running: {}", agent)).size(11.0));
                if state.cancelled {
                    ui.label(egui::RichText::new("Cancelled").color(egui::Color32::RED).size(11.0));
                }
            });
            ui.add_space(4.0);
        }

        // List entries
        ui.add_space(4.0);
        for (idx, entry) in state.entries.iter().enumerate() {
            self.draw_chain_entry(ui, entry);

            // Toggle expansion on click
            let btn_text = if self.agent_chain_expanded.contains(&idx) { "▼" } else { "▶" };
            if ui.small_button(btn_text).clicked() {
                if self.agent_chain_expanded.contains(&idx) {
                    self.agent_chain_expanded.retain(|&i| i != idx);
                } else {
                    self.agent_chain_expanded.push(idx);
                }
            }
            if self.agent_chain_expanded.contains(&idx) {
                self.draw_chain_entry_expanded(ui, entry);
            }
            ui.add_space(2.0);
        }
    }

    /// Render a single chain entry with icon, name, and depth.
    fn draw_chain_entry(&self, ui: &mut egui::Ui, entry: &AgentChainEntry) {
        let icon = if entry.error.is_some() { "❌" } else { "✅" };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
            ui.label(egui::RichText::new(icon).size(10.0));
            ui.label(egui::RichText::new(&entry.agent_name).size(10.0));
            ui.label(egui::RichText::new(format!("depth={}", entry.depth)).size(10.0));
        });
    }

    /// Render expanded entry with truncated result preview.
    fn draw_chain_entry_expanded(&self, ui: &mut egui::Ui, entry: &AgentChainEntry) {
        let preview = if entry.result.len() > 200 {
            format!("{}...", &entry.result[..200])
        } else {
            entry.result.clone()
        };
        if let Some(ref err) = entry.error {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", err));
            });
        } else {
            ui.label(egui::RichText::new(preview).size(10.0));
        }
        ui.add_space(2.0);
    }

}
