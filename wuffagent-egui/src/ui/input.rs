use std::sync::{Arc, Mutex};

use eframe::egui;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus, MessageKind};
use super::theme::Theme;

impl ChatApp {
    pub(super) fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
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
        self.status = AppStatus::Generating;
        self.chat.status = AppStatus::Generating;

        // Add user message to chat display
        let image = self.chat.pending_image.take();
        self.chat.messages.push(crate::types::ChatMessage {
            kind: MessageKind::Normal,
            role: "user".to_string(),
            content: input.clone(),
            timestamp: crate::types::format_timestamp(),
            image: image.map(|_| String::new()),
        });

        // Get tool definitions
        let tool_defs = self.tool_manager.get_tool_definitions();

        // Apply the selected agent profile's system prompt for this request
        // (None = "Auto" → the general profile).
        let agent_prompt = match self.selected_agent_index {
            Some(i) => {
                let name = self.get_agent_names().get(i).cloned().unwrap_or_default();
                self.load_agent_system_prompt(&[name.as_str()])
            }
            None => {
                self.load_agent_system_prompt(&["general", "generalist"])
            }
        };
        if !agent_prompt.is_empty() {
            self.client.set_system_prompt(&agent_prompt);
            tracing::info!("Applied agent system prompt ({} chars)", agent_prompt.len());
        } else {
            tracing::warn!("No agent system prompt found — chat will run without one");
        }

        // Cancel any in-flight engine (previous send)
        if let Some(ref engine) = self.chat_engine {
            engine.cancel();
        }

        // Engine and client tool events both flow straight into the single
        // pending_tx the UI polls each frame — no relay task needed.
        let pending_tx = match self.pending_tx.clone() {
            Some(tx) => tx.lock().unwrap().clone(),
            None => return,
        };
        self.client.set_tool_event_sender(pending_tx.clone());

        // Create and start the chat engine
        let client = Arc::new(Mutex::new(self.client.clone()));
        let tool_manager = (*self.tool_manager).clone();
        let engine = crate::client::engine::ChatEngine::new(client, tool_manager, pending_tx);
        // Store the engine for potential cancellation
        self.chat_engine = Some(engine);

        // Start the chat
        self.chat_engine.as_ref().unwrap().start_chat(input, tool_defs);
    }

    /// Load the system prompt of the first matching worker profile.
    /// Searches the same workers directories as `get_agent_names` and
    /// matches the profile's `name` field (not the file name).
    /// Returns an empty string when no candidate profile exists.
    fn load_agent_system_prompt(&self, names: &[&str]) -> String {
        let mut dirs = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join("workers"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                dirs.push(exe_dir.join("workers"));
            }
        }
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| self.config.file_path.clone());
        dirs.push(agents_dir.join("workers"));

        let mut prompt = String::new();
        for dir in dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(cfg) = serde_json::from_str::<crate::agents::WorkerConfig>(&content) {
                            if names.iter().any(|n| cfg.name == *n) {
                                prompt = cfg.system_prompt;
                                break;
                            }
                        }
                    }
                }
            }
            if !prompt.is_empty() {
                break;
            }
        }

        // Append available subagent hint so the model knows when to delegate.
        if let Some(names) = self.agent_engine.available_agent_names() {
            if !names.is_empty() {
                prompt.push_str(&format!(
                    "\n\nYou can delegate tasks to other agents using the agent_call tool. Available agents: {}.",
                    names
                ));
            }
        }

        prompt
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
        self.status = AppStatus::Generating;
        self.chat.status = AppStatus::Generating;
        self.chat.messages.push(crate::types::ChatMessage {
            kind: MessageKind::Normal,
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
        tracing::info!("[CANCEL] Stopping all generation");

        // Cancel the chat engine and agent engine
        if let Some(ref engine) = self.chat_engine {
            engine.cancel();
        }
        self.agent_cancel_token.cancel();

        // Mark the chain as cancelled so the UI reflects it immediately
        self.agent_chain_state.cancelled = true;

        // Stop generating state
        self.chat.is_generating = false;
        self.chat.status = AppStatus::Ready;
        self.chat.pending_error = None;
    }
}
