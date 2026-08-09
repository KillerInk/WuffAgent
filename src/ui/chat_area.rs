use eframe::egui;

use super::window::{ChatApp, ChatMessage};

impl ChatApp {
    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        // Show pending error as inline warning
        if let Some(ref err) = self.pending_error {
            let err_clone = err.clone();
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", err_clone));
                if ui.button("Dismiss").clicked() {
                    self.pending_error = None;
                }
            });
            ui.separator();
        }

        // Clone messages to avoid borrow checker issues
        let messages: Vec<ChatMessage> = self.chat_display.clone();

        egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    for msg in &messages {
                        self.draw_message(ui, msg);
                    }

                    // Show current streaming response
                    if self.is_generating && !self.current_response.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label("AI:  ");
                            ui.label(
                                egui::RichText::new(&self.current_response)
                                    .color(egui::Color32::from_rgb(150, 200, 150)),
                            );
                            ui.spinner();
                        });
                    } else if self.is_generating && self.current_response.is_empty() {
                        // Show spinner while waiting for first chunk
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label("AI:  ");
                            ui.spinner();
                        });
                    }
                });
            });
    }

    pub(super) fn draw_message(&self, ui: &mut egui::Ui, message: &ChatMessage) {
        ui.horizontal(|ui| {
            if message.role == "user" {
                ui.label("You: ");
            } else {
                ui.label("AI: ");
            }
            ui.label(&message.content);
        });
    }
}
