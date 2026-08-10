use eframe::egui;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;

use crate::client::ChatClient;

use super::window::{AppEvent, AppStatus, ChatApp};

impl ChatApp {
    pub(super) fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        ui.style_mut().spacing.item_spacing.y = 0.0;

        // Validate input length
        const MAX_MESSAGE_LENGTH: usize = 4000;
        let input_len = self.input_text.len();
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
        if self.pending_image.is_some() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("📷 Image attached").size(11.0).color(egui::Color32::GRAY));
                if ui.button("✕").clicked() {
                    self.pending_image = None;
                }
            });
            ui.separator();
        }

        // Input takes remaining space, button stays visible
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            const BUTTON_WIDTH: f32 = 55.0;
            let input_width = (ui.available_width() - BUTTON_WIDTH - 4.0).max(0.0);
            let text_edit = egui::TextEdit::singleline(&mut self.input_text);
            ui.add_sized([input_width, 22.0], text_edit);
            if !self.is_generating {
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
                    self.pending_image = Some(base64_img);
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
        let text = self.input_text.trim().to_string();
        if let Err(e) = self.validate_input(&text) {
            self.pending_error = Some(e);
            return;
        }

        self.input_text.clear();
        let image = self.pending_image.take();
        self.add_message_with_image("user", &text, image);

        // Transition to generating state
        self.is_generating = true;
        self.current_response.clear();
        self.status = AppStatus::Generating;

        // Cancel any existing streaming task
        if let Some(task) = self.streaming_task.take() {
            task.abort();
        }

        let text_clone = text.clone();
        let tx = self.pending_tx.as_ref().unwrap().clone();
        let client = self.client.clone();
        let streaming = self.streaming;
        let tool_defs = self.tool_manager.get_tool_definitions();

        let handle = tokio::spawn(async move {
            if streaming {
                Self::do_streaming(client, text_clone, tx, tool_defs).await;
            } else {
                Self::do_send_message(client, text_clone, tx, tool_defs).await;
            }
        });

        self.streaming_task = Some(handle);
    }

    async fn do_streaming(
        client: Arc<Mutex<ChatClient>>,
        text: String,
        tx: mpsc::Sender<AppEvent>,
        tool_defs: Vec<crate::tools::ToolDefinition>,
    ) {
        let tx_clone = tx.clone();
        let client_clone = client.lock().unwrap().clone();
        let tools_ref: Vec<crate::tools::ToolDefinition> = tool_defs;
        let result = client_clone
            .stream_message_with_tools_and_usage(&text, Some(&tools_ref), move |chunk| {
                let _ = tx_clone.send(AppEvent::StreamChunk {
                    content: chunk.clone(),
                });
                Ok(())
            })
            .await;

        match result {
            Ok(usage) => {
                let content = {
                    let c = client_clone.conversation().lock().unwrap();
                    c.last().map(|m| m.content.clone())
                };
                if let Some(content) = content {
                    let _ = tx.send(AppEvent::StreamComplete { content, usage });
                }
                
                // Check for malformed tool calls and send warnings
                let warnings = client_clone.check_tool_call_warnings();
                for (tool_name, message) in warnings {
                    let _ = tx.send(AppEvent::ToolCallWarning { tool_name, message });
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::StreamError { error: e.to_string() });
            }
        }
    }

    async fn do_send_message(
        client: Arc<Mutex<ChatClient>>,
        text: String,
        tx: mpsc::Sender<AppEvent>,
        tool_defs: Vec<crate::tools::ToolDefinition>,
    ) {
        let client_clone = client.lock().unwrap().clone();
        let tools_ref: Vec<crate::tools::ToolDefinition> = tool_defs;
        let result = client_clone
            .send_message_with_tools(&text, Some(&tools_ref))
            .await;

        let _ = tx.send(match result {
            Ok((content, usage)) => AppEvent::MessageResult { content, usage },
            Err(e) => AppEvent::MessageError { error: e.to_string() },
        });
    }

    pub(super) fn stop_generation(&mut self) {
        if let Some(task) = self.streaming_task.take() {
            task.abort();
        }
        self.is_generating = false;
        self.current_response.clear();
        self.status = AppStatus::Ready;
    }
}
