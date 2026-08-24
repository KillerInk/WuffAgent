use std::sync::{Arc, Mutex};

use eframe::egui;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus};
use super::theme::Theme;
use crate::ui::state::EngineEvent;

impl ChatApp {
    pub(super) fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.clone().theme.clone());
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
                // Agent selector dropdown
                let agent_names = self.get_agent_names();
                let selected = self.selected_agent_index;
                let selected_label = selected
                    .map(|i| agent_names.get(i).cloned().unwrap_or_default())
                    .unwrap_or_else(|| "Auto".to_string());
                let mut next_idx = selected;
                egui::ComboBox::from_id_salt("agent_selector")
                    .width(120.0)
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut next_idx, None, "Auto");
                        for (i, name) in agent_names.iter().enumerate() {
                            ui.selectable_value(&mut next_idx, Some(i), name);
                        }
                        if agent_names.is_empty() {
                            ui.label(egui::RichText::new("No agents found").size(10.0).color(theme.text_secondary));
                        }
                    });
                if next_idx != selected {
                    self.selected_agent_index = next_idx;
                }
                ui.add_space(6.0);

                // Reasoning effort dropdown (applied immediately on change)
                egui::ComboBox::from_id_salt("reasoning_effort")
                    .width(130.0)
                    .selected_text(self.reasoning_effort.label())
                    .show_ui(ui, |ui| {
                        for variant in crate::types::ReasoningEffort::VARIANTS {
                            if ui
                                .selectable_value(&mut self.reasoning_effort, variant, variant.label())
                                .changed()
                            {
                                self.client.set_reasoning_effort(self.reasoning_effort);
                                tracing::info!(
                                    "Reasoning effort changed to {:?}",
                                    self.reasoning_effort
                                );
                            }
                        }
                    });
                ui.add_space(6.0);

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
        let input = self.chat.input_text.trim().to_string();
        if let Err(e) = self.validate_input(&input) {
            self.chat.status = AppStatus::Error(e.clone());
            self.chat.pending_error = Some(e);
            return;
        }

        tracing::info!("[CHAT PATH] send_message called with: {}", input);

        self.chat.input_text.clear();
        self.chat.is_generating = true;
        self.chat.is_streaming = true;
        self.chat.streaming = true;
        self.status = AppStatus::Generating;
        self.chat.status = AppStatus::Generating;

        // Add user message to chat display
        let image = self.chat.pending_image.take();
        self.chat.messages.push(crate::types::ChatMessage {
            role: "user".to_string(),
            content: input.clone(),
            timestamp: crate::types::format_timestamp(),
            image: image.map(|_| String::new()),
        });

        // Get tool definitions
        let tool_defs = self.tool_manager.get_tool_definitions();

        // Abort any existing streaming task
        if let Some(handle) = self.chat.streaming_task.take() {
            handle.abort();
        }

        // Create a channel for engine events (engine → relay task)
        let (engine_tx, engine_rx) = std::sync::mpsc::channel::<EngineEvent>();

        // Create a dedicated channel for client tool events (client → UI).
        // We need a separate channel because the relay task also writes to pending_tx,
        // and the UI must read from a single receiver.
        let (tool_tx, tool_rx) = std::sync::mpsc::channel::<AppEvent>();

        // Spawn a task that merges both streams into pending_tx
        if let Some(pending_tx) = self.pending_tx.clone() {
            let handle = tokio::spawn(async move {
                // We need to select from both receivers. Use a simple polling loop.
                let engine_rx = std::sync::Mutex::new(engine_rx);
                let tool_rx = std::sync::Mutex::new(tool_rx);
                let pending_tx = pending_tx.clone();

                loop {
                    let mut ready = false;
                    // Try engine_rx
                    if let Ok(rx) = engine_rx.lock() {
                        if let Ok(event) = rx.try_recv() {
                            let app_event: AppEvent = event.into();
                            let _ = pending_tx.lock().unwrap().send(app_event);
                            ready = true;
                        }
                    }
                    // Try tool_rx
                    if let Ok(rx) = tool_rx.lock() {
                        if let Ok(event) = rx.try_recv() {
                            let _ = pending_tx.lock().unwrap().send(event);
                            ready = true;
                        }
                    }
                    if !ready {
                        // Both channels empty, wait a bit before polling again
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            });
            self.chat.streaming_task = Some(handle);
        }

        // Wire the tool event sender into the client so tool execution events
        // (ToolCallStart/Complete/Error) reach the UI.
        self.client.set_tool_event_sender(tool_tx);

        // Create and start the chat engine
        let client = Arc::new(Mutex::new(self.client.clone()));
        let tool_manager = (*self.tool_manager).clone();
        let engine = crate::client::engine::ChatEngine::new(
            client,
            tool_manager,
            engine_tx,
        );
        // Store the engine for potential cancellation
        self.chat_engine = Some(engine);

        // Start the chat
        self.chat_engine.as_ref().unwrap().start_chat(input, tool_defs);
    }

    /// Return the list of agent names from all known workers directories.
    fn get_agent_names(&self) -> Vec<String> {
        let config_path = &self.config.file_path;
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Scan project-level workers/ directory (relative to cwd or exe)
        if let Ok(cwd) = std::env::current_dir() {
            let workers_dir = cwd.join("workers");
            if workers_dir.exists() {
                if let Ok(workers) = crate::agents::WorkerConfig::load_all_from_dir(&workers_dir) {
                    for w in workers {
                        if seen.insert(w.name.clone()) {
                            names.push(w.name);
                        }
                    }
                }
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let workers_dir = exe_dir.join("workers");
                if workers_dir.exists() {
                    if let Ok(workers) = crate::agents::WorkerConfig::load_all_from_dir(&workers_dir) {
                        for w in workers {
                            if seen.insert(w.name.clone()) {
                                names.push(w.name);
                            }
                        }
                    }
                }
            }
        }

        // Also scan the config-directory workers subdirectory
        let agents_dir = config_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| config_path.clone());
        let config_workers = agents_dir.join("workers");
        if config_workers.exists() {
            if let Ok(workers) = crate::agents::WorkerConfig::load_all_from_dir(&config_workers) {
                for w in workers {
                    if seen.insert(w.name.clone()) {
                        names.push(w.name);
                    }
                }
            }
        }

        names
    }

    /// Send a /plan request to the agent engine.
    pub(super) fn send_plan_request(&mut self, request: &str) {
        tracing::info!("[AGENT ENGINE] send_plan_request called with: {}", request);

        self.chat.input_text.clear();
        self.chat.is_generating = true;
        self.chat.is_streaming = true;
        self.chat.streaming = true;
        self.status = AppStatus::Generating;
        self.chat.status = AppStatus::Generating;
        self.chat.messages.push(crate::types::ChatMessage {
            role: "user".to_string(),
            content: format!("/plan {}", request),
            timestamp: crate::types::format_timestamp(),
            image: None,
        });

        // Reset agent chain state
        self.agent_chain_state = super::state::AgentChainState::default();
        self.agent_chain_state.active = true;
        self.chat.is_pipeline_running = true;

        // Clone dependencies
        let cancel_token = self.agent_cancel_token.clone();
        let event_tx = self.pending_tx.clone();
        let request = request.to_string();

        // Wire the event tx so the engine can emit chain events, and apply
        // the current reasoning effort to the engine's LLM client.
        let engine = if let Some(ref tx) = self.pending_tx {
            let inner_tx = tx.lock().unwrap().clone();
            let inner = (*self.agent_engine).clone();
            Arc::new(
                inner
                    .with_event_tx(Arc::new(Mutex::new(inner_tx)))
                    .with_reasoning_effort(self.reasoning_effort),
            )
        } else {
            Arc::new((*self.agent_engine).clone().with_reasoning_effort(self.reasoning_effort))
        };

        // Clone event_tx for the async block
        let event_tx_for_spawn = event_tx.clone();

        // Spawn async task
        tokio::spawn(async move {
            tracing::info!("[AGENT ENGINE] Running agent engine for: {}", request);

            let result = tokio::select! {
                result = engine.execute(&request, &cancel_token) => result,
                _ = cancel_token.cancelled() => {
                    Ok(String::from("[CANCELLED]"))
                }
            };

            if let Some(ref tx) = event_tx_for_spawn {
                match &result {
                    Ok(response) => {
                        tracing::info!("[AGENT ENGINE] Completed with {} chars", response.len());
                        let _ = tx.lock().unwrap().send(AppEvent::AgentEngineComplete {
                            response: response.clone(),
                        });
                    }
                    Err(e) => {
                        tracing::error!("[AGENT ENGINE] Failed: {}", e);
                        let _ = tx.lock().unwrap().send(AppEvent::AgentEngineError {
                            error: e.clone(),
                        });
                    }
                }
                let _ = tx.lock().unwrap().send(AppEvent::AgentEngineStopped);
            }
        });
    }

    pub(super) fn stop_generation(&mut self) {
        // Also cancel the streaming task
        if let Some(handle) = self.chat.streaming_task.take() {
            handle.abort();
        }

        tracing::info!("[CANCEL] Stopping all generation");

        // Cancel agent engine
        self.agent_cancel_token.cancel();

        // Mark the chain as cancelled so the UI reflects it immediately
        self.agent_chain_state.cancelled = true;

        // Stop streaming state
        self.chat.is_generating = false;
        self.chat.is_streaming = false;
        self.chat.streaming = false;
        self.chat.status = AppStatus::Ready;
        self.chat.pending_error = None;
    }
}
