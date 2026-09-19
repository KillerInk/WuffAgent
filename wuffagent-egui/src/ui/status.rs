use eframe::egui;

use super::state::ChatApp;
use crate::types::AppStatus;
use super::theme::Theme;

impl ChatApp {
    pub(super) fn draw_status_bar(&self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
        let chat = match self.selected_chat_state() {
            Some(c) => c,
            None => return,
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
             
            // Status indicator with colored dot
            let status_color = match &chat.status {
                AppStatus::Stopped => theme.text_secondary,
                AppStatus::Connecting => theme.warning,
                AppStatus::Ready => theme.success,
                AppStatus::Generating => theme.primary,
                AppStatus::Error(_) => theme.error,
            };
            let status_text = match &chat.status {
                AppStatus::Stopped => "● Stopped",
                AppStatus::Connecting => "● Connecting...",
                AppStatus::Ready => "● Ready",
                AppStatus::Generating => "● Generating...",
                AppStatus::Error(_) => return,
            };
            ui.label(egui::RichText::new(status_text).color(status_color).size(11.0));

            if chat.is_generating {
                ui.label(egui::RichText::new("Streaming").color(theme.accent).size(11.0));
            }
             
            ui.separator();
            ui.label(egui::RichText::new(format!("Messages: {}", chat.messages.len())).color(theme.text_secondary).size(11.0));

            // Memory count indicator with a tooltip listing project + threshold.
            let mem_count = self.memory_manager.count();
            let mconfig = self.memory_manager.config();
            ui.label(egui::RichText::new(format!("🧠 {}", mem_count)).color(theme.text_secondary).size(11.0))
                .on_hover_text(format!(
                    "{} active memories (project '{}', max {})\nMaintenance: {}",
                    mem_count,
                    mconfig.project,
                    mconfig.max_entries,
                    if mconfig.memory_maintenance {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));

            // MCP indicator: connected / total servers.
            let mcp_servers = self.mcp_manager.snapshot();
            if !mcp_servers.is_empty() {
                let (connected, total) = self.mcp_manager.connected_counts();
                let detail: Vec<String> = mcp_servers
                    .iter()
                    .map(|s| {
                        let tool_count = s.tools.iter().filter(|t| t.enabled).count();
                        format!("{}: {:?} ({} enabled tools)", s.name, s.status, tool_count)
                    })
                    .collect();
                ui.label(
                    egui::RichText::new(format!("MCP {connected}/{total}"))
                        .color(if connected == total {
                            theme.success
                        } else if connected == 0 {
                            theme.text_dim
                        } else {
                            theme.warning
                        })
                        .size(11.0),
                )
                .on_hover_text(detail.join("\n"));
            }
        });
    }

    /// Returns the effective n_ctx for trim/gauge budgeting.
    ///
    /// Prefers the value the connected server reports via `/props` (fetched in
    /// BOTH local and remote mode — the server is the source of truth). While
    /// that value has not been fetched yet (0), we return 0 — NOT a fallback to
    /// the local config. Substituting the config value here was the bug that
    /// made the server's real limit invisible: the trimmer ran on the wrong
    /// (4096) budget even though the server allowed more. A 0 return safely
    /// disables trimming (`n_ctx() > 0` guards in the agent/client loop) until
    /// the real value arrives.
    pub(super) fn get_effective_n_ctx(&self) -> u32 {
        self.remote_n_ctx
    }

    /// Returns true when the connected server's /props n_ctx has not been
    /// fetched yet. Used to show "…" in the bottom bar instead of a misleading
    /// value.
    pub(super) fn remote_props_unknown(&self) -> bool {
        self.remote_n_ctx == 0
    }

    pub(super) fn draw_bottom_bar(&self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
        let n_ctx = self.get_effective_n_ctx();
        let n_gpu_layers = self.server.get_n_gpu_layers();
        let threads = self.server.get_threads();
        let chat = match self.selected_chat_state() {
            Some(c) => c,
            None => return,
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
             
            // Token count pill
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Tokens: {}", chat.token_count))
                    .color(theme.text_secondary)
                    .size(11.0)
            ).wrap());
             
            // Context usage pill with color coding
            let context_color = if chat.context_used > 80.0 {
                theme.error
            } else if chat.context_used > 60.0 {
                theme.warning
            } else {
                theme.success
            };
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Ctx: {:.1}%", chat.context_used))
                    .color(context_color)
                    .size(11.0)
            ).wrap());
            
            ui.separator();
            
            // Server specs. In remote mode before /props has been fetched,
            // show "…" rather than the local config's n_ctx (which is not the
            // remote server's limit and would be misleading).
            let ctx_label = if self.remote_props_unknown() {
                "…".to_string()
            } else {
                n_ctx.to_string()
            };
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Ctx: {} | GPU: {} | Threads: {}", ctx_label, n_gpu_layers, threads))
                    .color(theme.text_dim)
                    .size(11.0)
            ).wrap());
        });
    }
}
