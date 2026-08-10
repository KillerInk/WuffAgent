use eframe::egui;

use super::state::ChatApp;
use super::window::{AppEvent, AppStatus};

impl ChatApp {
    pub(super) fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        ui.style_mut().spacing.item_spacing.y = 0.0;

        // Validate input length
        const MAX_MESSAGE_LENGTH: usize = 4000;
        let input_len = self.chat.input_text.len();
        if input_len > MAX_MESSAGE_LENGTH {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("Message too long (max {} characters, current: {})", MAX_MESSAGE_LENGTH, input_len),
                );
            });
            ui.separator();
        }

        // Show pending image preview
        if self.chat.pending_image.is_some() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📷 Image attached").size(11.0).color(egui::Color32::GRAY));
                if ui.button("✕").clicked() {
                    self.chat.pending_image = None;
                }
            });
            ui.separator();
        }

        // Input takes remaining space, button stays visible
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            const BUTTON_WIDTH: f32 = 55.0;
            let input_width = (ui.available_width() - BUTTON_WIDTH - 4.0).max(0.0);
            let text_edit = egui::TextEdit::singleline(&mut self.chat.input_text);
            ui.add_sized([input_width, 22.0], text_edit);
            if !self.chat.is_generating {
                if ui.button("Send").clicked() {
                    self.send_message();
                }
            } else {
                if ui.button("Stop").clicked() {
                    self.stop_generation();
                }
            }
        });
        
        // Handle drag-and-drop for images
        self.handle_image_drop(ui);
    }
    
    fn handle_image_drop(&mut self, ui: &mut egui::Ui) {
        // Use egui's built-in drop target for files
        let drop_zone = ui.allocate_space(egui::Vec2::new(ui.available_width(), 20.0));
        
        // Check for drop events using egui's drop target API
        let response = ui.interact(drop_zone.1, ui.id(), egui::Sense::click());
        
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        
        // Check for dropped files
        if let Some(drop) = ui.input(|i| i.raw.dropped_files.clone().into_iter().next()) {
            if let Some(path) = drop.path {
                self.process_dropped_file(path);
            }
        }
    }
    
    fn process_dropped_file(&mut self, file_path: std::path::PathBuf) {
        // Check if it's an image file
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let is_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff");
        
        if is_image {
            if let Ok(bytes) = std::fs::read(&file_path) {
                // Validate it's actually an image by trying to decode
                if image::ImageFormat::from_extension(&ext).is_some() {
                    let base64_img = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
                    self.chat.pending_image = Some(base64_img);
                    tracing::info!("Dropped image: {:?}", file_path);
                }
            }
        }
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

        // Clone tool definitions for the async task
        let tool_defs = self.get_tool_definitions();
        let client = self.client.clone();
        let tx = self.pending_tx.clone();

        // Use spawn_local since we're in a single-threaded Tokio runtime
        // and ChatClient is not Send (contains non-Send Mutex guards)
        let handle = tokio::task::spawn_local(async move {
            let cl = client.lock().unwrap();
            let result = cl.send_message_with_tools(&input, Some(&tool_defs)).await;
            drop(cl);
            
            match result {
                Ok((content, usage)) => {
                    if let Some(tx) = &tx {
                        let _ = tx.send(AppEvent::StreamComplete { content, usage });
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to send message: {}", e);
                    if let Some(tx) = &tx {
                        let _ = tx.send(AppEvent::StreamError { error: e.to_string() });
                    }
                }
            }
        });

        // Store the handle for potential cancellation
        self.chat.streaming_task = Some(handle);
    }

    pub(super) fn stop_generation(&mut self) {
        // Cancel the streaming task if it exists
        if let Some(handle) = self.chat.streaming_task.take() {
            handle.abort();
        }
        self.stop_streaming();
    }
}
