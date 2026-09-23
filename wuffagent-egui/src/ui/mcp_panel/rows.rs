//! Server-row + tool-row rendering (moved from `mcp_panel.rs`, C3).

use std::sync::Arc;

use eframe::egui;

use wuffagent_core::config::{Config, McpServerConfig};
use wuffagent_core::tools::mcp::{McpManager, McpServerSnapshot, McpServerStatus, McpToolSnapshot};

use super::edit_state::{truncate, ServerEditState};
use super::{McpOp, McpPanel};
use crate::ui::theme::Theme;

impl McpPanel {
    /// One server row: status, name, enable toggle, actions, expandable tools.
    pub(super) fn draw_server_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        snap: &McpServerSnapshot,
        mcp: &Arc<McpManager>,
        config: &mut Config,
    ) {
        let (dot, status_text, status_color) = match &snap.status {
            McpServerStatus::Connected { .. } => ("●", "connected", theme.success),
            McpServerStatus::Connecting => ("◌", "connecting…", theme.accent),
            McpServerStatus::Configured => {
                if snap.enabled {
                    ("○", "configured", theme.text_secondary)
                } else {
                    ("⊘", "disabled", theme.text_dim)
                }
            }
            McpServerStatus::Error(_) => ("⚠", "error", theme.warning),
        };

        let is_expanded = self.expanded.contains(&snap.name);
        let fill = if is_expanded {
            theme.surface_light
        } else {
            theme.surface
        };

        egui::Frame::new()
            .fill(fill)
            .corner_radius(4)
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(dot).color(status_color).strong());
                    ui.label(
                        egui::RichText::new(format!("{status_text} ·"))
                            .color(status_color)
                            .small(),
                    );
                    // Click the name to expand/collapse the tool list.
                    if ui
                        .label(
                            egui::RichText::new(&snap.name)
                                .color(theme.text_primary)
                                .strong(),
                        )
                        .clicked()
                    {
                        if let Some(pos) = self.expanded.iter().position(|n| n == &snap.name) {
                            self.expanded.remove(pos);
                        } else {
                            self.expanded.push(snap.name.clone());
                        }
                    }
                    ui.label(
                        egui::RichText::new(truncate(&snap.transport_summary, 48))
                            .color(theme.text_dim)
                            .small(),
                    );
                    if let McpServerStatus::Error(err) = &snap.status {
                        ui.label(
                            egui::RichText::new(truncate(err, 60))
                                .color(theme.warning)
                                .small(),
                        );
                    }
                });

                // Controls row
                let busy = self.job.is_some();
                let name = snap.name.clone();
                ui.horizontal(|ui| {
                    // Enable/disable (server-level; connects/disconnects to
                    // match, so it runs as a blocking job).
                    let mut enabled = snap.enabled;
                    if ui.add(egui::Checkbox::new(&mut enabled, "enabled")).changed() {
                        self.start_job(mcp, McpOp::SetEnabled(name.clone(), enabled));
                        if enabled {
                            if let Some(slot) = config.mcp_servers.iter_mut().find(|c| c.name == name) {
                                slot.enabled = true;
                            }
                            self.persist(config, format!("✓ {name} enabled"));
                        } else {
                            if let Some(slot) = config.mcp_servers.iter_mut().find(|c| c.name == name) {
                                slot.enabled = false;
                            }
                            self.persist(config, format!("✓ {name} disabled"));
                        }
                    }

                    match snap.status {
                        McpServerStatus::Connected { .. } => {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Disconnect").fill(theme.surface_light))
                                .clicked()
                            {
                                self.start_job(mcp, McpOp::Disconnect(name.clone()));
                            }
                            if ui
                                .add_enabled(!busy, egui::Button::new("↻ Refresh tools").fill(theme.surface_light))
                                .clicked()
                            {
                                self.start_job(mcp, McpOp::Refresh(name.clone()));
                            }
                        }
                        McpServerStatus::Connecting => {}
                        McpServerStatus::Configured | McpServerStatus::Error(_) => {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Connect").fill(theme.primary))
                                .clicked()
                            {
                                self.start_job(mcp, McpOp::Connect(name.clone()));
                            }
                        }
                    }

                    if ui
                        .add_enabled(!busy, egui::Button::new("Edit").fill(theme.surface_light))
                        .clicked()
                    {
                        let existing = config
                            .mcp_servers
                            .iter()
                            .find(|c| c.name == name)
                            .cloned()
                            .unwrap_or_else(|| McpServerConfig {
                                name: name.clone(),
                                ..Default::default()
                            });
                        self.editing = Some(ServerEditState::from_config(existing));
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Delete").fill(theme.surface_light))
                        .clicked()
                    {
                        self.pending_delete = Some(name);
                    }
                });

                // Expanded tool list
                if is_expanded {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Tools").color(theme.text_secondary).strong());
                    if snap.tools.is_empty() {
                        ui.label(
                            egui::RichText::new("No tools discovered (connect, then refresh).")
                                .color(theme.text_dim)
                                .small(),
                        );
                    } else {
                        for tool in &snap.tools {
                            self.draw_tool_row(ui, theme, &snap.name, tool, mcp);
                        }
                    }
                }
            });

        // Delete confirmation dialog (driven by pending_delete == this name).
        if self.pending_delete.as_deref() == Some(&snap.name) {
            let name = snap.name.clone();
            let mut confirmed = false;
            egui::Window::new("Confirm delete")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!("Remove MCP server '{name}'?"));
                    ui.label(
                        egui::RichText::new(
                            "Its tools will be unregistered and the process (if any) killed.",
                        )
                        .color(theme.text_secondary),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.pending_delete = None;
                        }
                        if ui.add(egui::Button::new("Delete").fill(theme.error)).clicked() {
                            confirmed = true;
                        }
                    });
                });
            if confirmed {
                self.pending_delete = None;
                self.start_job(mcp, McpOp::Remove(name));
            }
        }
    }

    /// A discovered tool with a per-tool enable checkbox.
    pub(super) fn draw_tool_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        server: &str,
        tool: &McpToolSnapshot,
        mcp: &Arc<McpManager>,
    ) {
        let mut enabled = tool.enabled;
        let mut changed = false;
        ui.horizontal(|ui| {
            changed = ui.add(egui::Checkbox::new(&mut enabled, "")).changed();
            ui.label(
                egui::RichText::new(&tool.name)
                    .monospace()
                    .color(theme.text_primary),
            );
            if !tool.description.is_empty() {
                ui.label(
                    egui::RichText::new(truncate(&tool.description, 60))
                        .color(theme.text_dim)
                        .small(),
                );
            }
        });
        if changed {
            self.on_tool_toggled(server, &tool.name, enabled, mcp);
        }
    }

    /// Apply a tool enable/disable toggle (fast, non-blocking state change).
    pub(super) fn on_tool_toggled(&mut self, server: &str, tool: &str, enabled: bool, mcp: &McpManager) {
        if mcp.set_tool_enabled(server, tool, enabled).is_ok() {
            self.message = Some(format!(
                "✓ tool '{}' {} on '{}'",
                tool,
                if enabled { "enabled" } else { "disabled" },
                server
            ));
        }
    }
}
