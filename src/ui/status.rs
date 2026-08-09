use eframe::egui;

use super::window::{AppStatus, ChatApp};

impl ChatApp {
    pub(super) fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let status_text = match &self.status {
                AppStatus::Stopped => "● Stopped".to_string(),
                AppStatus::Connecting => "● Connecting...".to_string(),
                AppStatus::Ready => "● Ready".to_string(),
                AppStatus::Generating => "● Generating...".to_string(),
                AppStatus::Error(e) => format!("● Error: {}", e),
            };
            let status_color = match &self.status {
                AppStatus::Stopped => egui::Color32::GRAY,
                AppStatus::Connecting => egui::Color32::BLUE,
                AppStatus::Ready => egui::Color32::GREEN,
                AppStatus::Generating => egui::Color32::BLUE,
                AppStatus::Error(_) => egui::Color32::RED,
            };
            ui.label(egui::RichText::new(&status_text).color(status_color));

            if self.streaming {
                ui.separator();
                ui.label("Streaming");
            }
            ui.separator();
            ui.label(format!("Messages: {}", self.chat_display.len()));
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
        let cfg = self.config.lock().unwrap();
        let is_remote = cfg.connection_type == crate::config::ConnectionType::Remote;
        drop(cfg);
        is_remote
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
        let n_ctx = self.get_effective_n_ctx();
        let n_gpu_layers = self.server.get_n_gpu_layers();
        let threads = self.server.get_threads();

        ui.horizontal(|ui| {
            ui.label("Tokens:");
            ui.label(self.token_count.to_string());
            ui.label(" | Context: ");
            ui.label(format!("{:.1}%", self.context_used));
            ui.separator();
            ui.label(format!("Ctx: {} | GPU: {} | Threads: {}", n_ctx, n_gpu_layers, threads));
        });
    }
}
