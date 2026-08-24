use eframe::egui;

use super::state::ChatApp;
use crate::types::AppStatus;
use super::theme::Theme;

impl ChatApp {
    pub(super) fn draw_status_bar(&self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
        
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            
            // Status indicator with colored dot
            let status_color = match &self.chat.status {
                AppStatus::Stopped => theme.text_secondary,
                AppStatus::Connecting => theme.warning,
                AppStatus::Ready => theme.success,
                AppStatus::Generating => theme.primary,
                AppStatus::Error(_) => theme.error,
            };
            let status_text = match &self.chat.status {
                AppStatus::Stopped => "● Stopped",
                AppStatus::Connecting => "● Connecting...",
                AppStatus::Ready => "● Ready",
                AppStatus::Generating => "● Generating...",
                AppStatus::Error(_) => return,
            };
            ui.label(egui::RichText::new(status_text).color(status_color).size(11.0));

            if self.chat.is_generating {
                ui.label(egui::RichText::new("Streaming").color(theme.accent).size(11.0));
            }
            
            ui.separator();
            ui.label(egui::RichText::new(format!("Messages: {}", self.chat.messages.len())).color(theme.text_secondary).size(11.0));
        });
    }

    /// Rough token count estimation: ~4 chars per token is a common rule of thumb
    pub(super) fn estimate_token_count(text: &str) -> u32 {
        if text.is_empty() {
            0
        } else {
            (text.len() as f32 / 4.0).ceil() as u32
        }
    }

    /// Returns true when in remote connection mode.
    pub(super) fn is_remote_mode(&self) -> bool {
        self.config.connection_type == crate::config::ConnectionType::Remote
    }

    /// Returns the effective n_ctx for display and percentage calculations.
    /// Local mode: uses the configured n_ctx (we control the server process).
    /// Remote mode: uses the n_ctx fetched from the remote server's /props endpoint.
    pub(super) fn get_effective_n_ctx(&self) -> u32 {
        if self.is_remote_mode() && self.remote_n_ctx > 0 {
            self.remote_n_ctx
        } else {
            self.server.get_n_ctx()
        }
    }

    pub(super) fn draw_bottom_bar(&self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
        let n_ctx = self.get_effective_n_ctx();
        let n_gpu_layers = self.server.get_n_gpu_layers();
        let threads = self.server.get_threads();

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            
            // Token count pill
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Tokens: {}", self.chat.token_count))
                    .color(theme.text_secondary)
                    .size(11.0)
            ).wrap());
            
            // Context usage pill with color coding
            let context_color = if self.chat.context_used > 80.0 {
                theme.error
            } else if self.chat.context_used > 60.0 {
                theme.warning
            } else {
                theme.success
            };
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Ctx: {:.1}%", self.chat.context_used))
                    .color(context_color)
                    .size(11.0)
            ).wrap());
            
            ui.separator();
            
            // Server specs
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Ctx: {} | GPU: {} | Threads: {}", n_ctx, n_gpu_layers, threads))
                    .color(theme.text_dim)
                    .size(11.0)
            ).wrap());
        });
    }
}
