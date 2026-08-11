use eframe::egui;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus};
use super::theme::Theme;
use crate::client::engine::EngineEvent;

impl ChatApp {
    pub(super) fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
        ui.style_mut().spacing.item_spacing.y = 0.0;

        // Validate input length
        const MAX_MESSAGE_LENGTH: usize = 4000;
        let input_len = self.chat.input_text.len();
        if input_len > MAX_MESSAGE_LENGTH {
            ui.horizontal(|ui| {
                ui.colored_label(
                    theme.error,
                    format!("Message too long (max {} characters, current: {})", MAX_MESSAGE_LENGTH, input_len),
                );
            });
        }

        // Image preview area
        if let Some(ref _image) = self.chat.pending_image {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📷 Image attached").size(11.0).color(theme.text_secondary));
                if ui.button("✕").clicked() {
                    self.chat.pending_image = None;
                }
            });
        }

        // Input row
        let input_width = ui.available_width() - 80.0; // Account for button
        ui.horizontal(|ui| {
            // Styled text input
            let text_edit = egui::TextEdit::singleline(&mut self.chat.input_text)
                .hint_text("Type a message...")
                .vertical_align(egui::Align::Center);
            let response = ui.add_sized([input_width, 32.0], text_edit);
            if response.lost_focus() && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter)) {
                if !self.chat.is_generating && !self.chat.input_text.trim().is_empty() {
                    self.send_message();
                }
            }

            // Send or Stop button
            if !self.chat.is_generating {
                let send_btn = egui::Button::new("Send")
                    .fill(theme.primary)
                    .rounding(6.0)
                    .min_size(egui::vec2(60.0, 28.0));
                if ui.add(send_btn).clicked() {
                    self.send_message();
                }
            } else {
                let stop_btn = egui::Button::new("Stop")
                    .fill(theme.error)
                    .rounding(6.0)
                    .min_size(egui::vec2(60.0, 28.0));
                if ui.add(stop_btn).clicked() {
                    self.stop_generation();
                }
            }
        });

        // Handle image drop - simplified
        let _drop_zone = ui.allocate_space(egui::Vec2::new(ui.available_width(), 10.0));
    }

    fn validate_input(&self, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("Message cannot be empty".to_string());
        }
        const MAX_MESSAGE_LENGTH: usize = 4000;
        if text.len() > MAX_MESSAGE_LENGTH {
            return Err(format!("Message too long (max {} characters)", MAX_MESSAGE_LENGTH));
        }
        Ok(())
    }

    pub(super) fn send_message(&mut self) {
        let input = self.chat.input_text.trim().to_string();
        if let Err(e) = self.validate_input(&input) {
            self.chat.status = AppStatus::Error(e.clone());
            self.chat.pending_error = Some(e);
            return;
        }

        self.chat.input_text.clear();
        self.start_streaming();

        // Add user message to chat display and session
        let image = self.chat.pending_image.take();
        self.add_message_with_image("user", &input, image);

        // Get tool definitions
        let tool_defs = self.get_tool_definitions();

        // Clone dependencies
        let client = self.client.clone();
        let tool_manager = self.tool_manager.clone();
        let event_tx = self.pending_tx.clone();

        // Create a channel for engine events
        let (engine_tx, engine_rx) = std::sync::mpsc::channel::<EngineEvent>();

        // Spawn a task to handle engine events
        if let Some(tx) = event_tx.clone() {
            let handle = tokio::spawn(async move {
                // Relay engine events to UI
                for event in engine_rx.iter() {
                    let app_event: AppEvent = event.into();
                    let _ = tx.send(app_event);
                }
            });
            self.chat.streaming_task = Some(handle);
        }

        // Create and start the chat engine
        let engine = crate::client::engine::ChatEngine::new(
            client,
            (*tool_manager).clone(),
            engine_tx,
        );
        
        // Store engine for potential cancellation
        self.chat.engine = Some(engine.clone());

        // Start the chat
        engine.start_chat(input, tool_defs);
    }

    pub(super) fn stop_generation(&mut self) {
        // Cancel the engine task
        if let Some(engine) = self.chat.engine.take() {
            engine.cancel();
        }
        
        // Also cancel the streaming task
        if let Some(handle) = self.chat.streaming_task.take() {
            handle.abort();
        }
        
        self.stop_streaming();
        self.chat.status = AppStatus::Ready;
        self.chat.pending_error = None;
    }
}
