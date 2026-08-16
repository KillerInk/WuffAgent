use eframe::egui;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus};
use super::theme::Theme;
use crate::ui::state::EngineEvent;

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

        // Input area: text field + button row below
        let input_width = ui.available_width(); // Full width — button is on its own row
        ui.vertical(|ui| {
            // Text input — constrained width, multiline
            ui.scope(|ui| {
                ui.set_max_width(input_width);
                let text_edit = egui::TextEdit::multiline(&mut self.chat.input_text)
                    .hint_text("Type a message... (use /plan to trigger multi-agent pipeline)")
                    .desired_width(f32::INFINITY);
                let response = ui.add(text_edit);
                // Send on Ctrl+Enter when focus is lost
                let modifiers = ui.ctx().input(|i| i.modifiers);
                if response.lost_focus()
                    && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter))
                    && modifiers.ctrl
                    && !self.chat.is_generating
                    && !self.chat.is_pipeline_running
                    && !self.chat.input_text.trim().is_empty()
                {
                    let input = self.chat.input_text.trim().to_string();
                    self.handle_send_input(&input);
                }
            });

            ui.add_space(6.0); // padding between text box and button

            // Button row below the text area
            ui.horizontal(|ui| {
                if !self.chat.is_generating && !self.chat.is_pipeline_running {
                    let send_btn = egui::Button::new("Send")
                        .fill(theme.primary)
                        .rounding(6.0)
                        .min_size(egui::vec2(60.0, 28.0));
                    if ui.add(send_btn).clicked() {
                        let input = self.chat.input_text.trim().to_string();
                        self.handle_send_input(&input);
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
        });

        // Handle image drop - simplified
        let _drop_zone = ui.allocate_space(egui::Vec2::new(ui.available_width(), 10.0));
    }

    fn handle_send_input(&mut self, input: &str) {
        if let Err(e) = self.validate_input(input) {
            self.chat.status = AppStatus::Error(e.clone());
            self.chat.pending_error = Some(e);
            return;
        }

        if let Some(rest) = input.strip_prefix("/plan") {
            let request = rest.trim().to_string();
            if request.is_empty() {
                self.chat.status = AppStatus::Error("Please provide a request after /plan".to_string());
                self.chat.pending_error = Some("Please provide a request after /plan".to_string());
            } else {
                self.send_plan_request(&request);
            }
        } else {
            self.send_message();
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
        self.chat.is_pipeline_running = false;
        let input = self.chat.input_text.trim().to_string();
        if let Err(e) = self.validate_input(&input) {
            self.chat.status = AppStatus::Error(e.clone());
            self.chat.pending_error = Some(e);
            return;
        }

        tracing::info!("[CHAT PATH] send_message called with: {}", input);

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

    /// Send a /plan request to the agent engine.
    pub(super) fn send_plan_request(&mut self, request: &str) {
        tracing::info!("[AGENT ENGINE] send_plan_request called with: {}", request);
        
        self.chat.input_text.clear();
        self.start_streaming();
        self.add_message("user", &format!("/plan {}", request));
        
        // Reset agent chain state
        self.agent_chain_state = super::state::AgentChainState::default();
        self.agent_chain_state.active = true;
        self.chat.is_pipeline_running = true;
        
        // Clone dependencies
        let engine = self.agent_engine.clone();
        let cancel_token = self.agent_cancel_token.clone();
        let event_tx = self.pending_tx.clone();
        let request = request.to_string();
        
        // Spawn async task
        tokio::spawn(async move {
            tracing::info!("[AGENT ENGINE] Running agent engine for: {}", request);
            
            // Execute with cancellation support
            let result = tokio::select! {
                result = engine.execute(&request, &cancel_token) => result,
                _ = cancel_token.cancelled() => {
                    Ok(String::from("[CANCELLED]"))
                }
            };
            
            match result {
                Ok(response) => {
                    tracing::info!("[AGENT ENGINE] Completed with {} chars", response.len());
                    if let Some(ref tx) = event_tx {
                        let _ = tx.send(AppEvent::AgentEngineComplete {
                            response,
                        });
                    }
                }
                Err(e) => {
                    tracing::error!("[AGENT ENGINE] Failed: {}", e);
                    if let Some(ref tx) = event_tx {
                        let _ = tx.send(AppEvent::AgentEngineError {
                            error: e.to_string(),
                        });
                    }
                }
            }
            
            // Reset pipeline running state
            if let Some(ref tx) = event_tx {
                let _ = tx.send(AppEvent::AgentEngineStopped);
            }
        });
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
        
        tracing::info!("[CANCEL] Stopping all generation");
        
        // Cancel agent engine
        self.agent_cancel_token.cancel();
        
        self.stop_streaming();
        self.chat.status = AppStatus::Ready;
        self.chat.pending_error = None;
    }
}
