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
        self.add_message("user", &text);

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

        let handle = tokio::spawn(async move {
            if streaming {
                Self::do_streaming(client, text_clone, tx).await;
            } else {
                Self::do_send_message(client, text_clone, tx).await;
            }
        });

        self.streaming_task = Some(handle);
    }

    async fn do_streaming(
        client: Arc<Mutex<ChatClient>>,
        text: String,
        tx: mpsc::Sender<AppEvent>,
    ) {
        let tx_clone = tx.clone();
        let client_clone = client.lock().unwrap().clone();
        let result = client_clone.stream_message_with_usage(&text, move |chunk| {
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
    ) {
        let client_clone = client.lock().unwrap().clone();
        let result = client_clone.send_message(&text).await;

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
