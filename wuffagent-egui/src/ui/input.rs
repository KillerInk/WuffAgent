use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use super::state::ChatApp;
use crate::types::{AppStatus, MessageKind};
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
                ui.label(egui::RichText::new("ðŸ“· Image attached").size(11.0).color(theme.text_secondary));
                if ui.button("âœ•").clicked() {
                    self.chat.pending_image = None;
                }
            });
        }

        // Input area: text field + button row below
        let input_width = ui.available_width(); // Full width â€” button is on its own row
        ui.vertical(|ui| {
            // Text input â€” constrained width, multiline
            ui.scope(|ui| {
                ui.set_max_width(input_width);
                let text_edit = egui::TextEdit::multiline(&mut self.chat.input_text)
                    .hint_text("Type a message...")
                    .desired_width(f32::INFINITY);
                let response = ui.add(text_edit);
                // Send on Ctrl+Enter
                let modifiers = ui.ctx().input(|i| i.modifiers);
                if response.has_focus()
                    && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter))
                    && modifiers.ctrl
                    && !self.chat.is_generating
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

                if !self.chat.is_generating {
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

        self.send_message();
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

        // Apply the selected agent profile's system prompt for this request
        // (None = "Auto" -> the general profile).
        let agent_prompt = match self.selected_agent_index {
            Some(i) => {
                let name = self.get_agent_names().get(i).cloned().unwrap_or_default();
                self.load_agent_system_prompt(&[name.as_str()])
            }
            None => {
                self.load_agent_system_prompt(&["general", "generalist"])
            }
        };
        if agent_prompt.is_empty() {
            tracing::warn!("No agent system prompt found - chat will run without one");
        }

        // Cancel any in-flight pipeline (previous send)
        if let Some(ref pipeline) = self.chat_pipeline {
            pipeline.cancel();
        }

        // Engine and client tool events both flow straight into the single
        // pending_tx the UI polls each frame - no relay task needed.
        let pending_tx = match self.pending_tx.clone() {
            Some(tx) => tx.lock().unwrap().clone(),
            None => return,
        };
        self.client.set_tool_event_sender(pending_tx.clone());

        // Sync the remote server's n_ctx to the client and engine before starting the chat loop.
        // Without this, the engine trims conversations to the config default (e.g. 4096)
        // instead of the server's actual context window.
        if self.remote_n_ctx > 0 {
            self.client = self.client.with_n_ctx(self.remote_n_ctx);
            self.agent_engine = Arc::new((*self.agent_engine).clone().with_n_ctx(self.remote_n_ctx));
        }

        // Load the agent session if one is configured so the agent starts with
        // context from a previous session. This is done here (on the engine) rather
        // than inside execute() because execute() takes &mut self and we need the
        // loaded conversation to be reflected in the engine before the pipeline starts.
        if let Some(sid) = self.agent_engine.agent_session_id() {
            let dir = self.agent_engine.agent_session_dir().clone();
            if !dir.as_os_str().is_empty() {
                let mut engine_clone = (*self.agent_engine).clone();
                if let Some(session) = crate::sessions::load_session(&dir, sid) {
                    let mut conv = self.client.conversation().lock().unwrap();
                    *conv = session.messages.clone();
                    drop(conv);
                    self.client.set_session(Some(sid.to_string()), dir.clone());
                    self.client.load_session();
                    // Also update the engine clone so execute() uses the right client
                    engine_clone.set_agent_session(Some(sid.to_string()), dir);
                    self.agent_engine = Arc::new(engine_clone);
                }
            }
        }

        // Create and start the chat pipeline
        let pipeline = crate::client::ChatPipeline::new(
            self.agent_engine.clone(),
            pending_tx,
            self.reasoning_effort,
        );
        // Store the pipeline for potential cancellation
        self.chat_pipeline = Some(pipeline);

        // Start the chat
        self.chat_pipeline.as_ref().unwrap().start(&input, &agent_prompt);
    }

    /// Load the system prompt of the first matching agent profile.
    /// Searches the same agents directories as `get_agent_names` and
    /// matches the profile's `name` field (not the file name).
    /// Returns an empty string when no candidate profile exists.
    fn load_agent_system_prompt(&self, names: &[&str]) -> String {
        let mut dirs = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join("agents"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                dirs.push(exe_dir.join("agents"));
            }
        }
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| self.config.file_path.clone());
        dirs.push(agents_dir.join("agents"));

        let mut prompt = String::new();
        for dir in dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(cfg) = serde_json::from_str::<crate::agents::config::AgentConfig>(&content) {
                            if names.iter().any(|n| cfg.name == *n) {
                                prompt = cfg.system_prompt;
                                break;
                            }
                        } else if let Ok(cfg) = serde_json::from_str::<crate::agents::config::WorkerConfig>(&content) {
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

    /// Return the list of agent names from all known agents directories.
    fn get_agent_names(&self) -> Vec<String> {
        let config_path = &self.config.file_path;
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Helper: scan a directory for agent names
        let mut scan_dir = |dir: PathBuf| {
            if dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(cfg) = serde_json::from_str::<crate::agents::config::AgentConfig>(&content) {
                                let _ = seen.insert(cfg.name.clone());
                                names.push(cfg.name);
                            } else if let Ok(cfg) = serde_json::from_str::<crate::agents::config::WorkerConfig>(&content) {
                                let _ = seen.insert(cfg.name.clone());
                                names.push(cfg.name);
                            }
                        }
                    }
                }
            }
        };

        // Scan project-level agents/ directory (relative to cwd or exe)
        if let Ok(cwd) = std::env::current_dir() {
            scan_dir(cwd.join("agents"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                scan_dir(exe_dir.join("agents"));
            }
        }

        // Also scan the config-directory workers subdirectory
        let agents_dir = config_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| config_path.clone());
        scan_dir(agents_dir.join("agents"));

        names
    }

    pub(super) fn stop_generation(&mut self) {
        tracing::info!("[CANCEL] Stopping all generation");

        // Cancel the chat pipeline and agent engine
        if let Some(ref pipeline) = self.chat_pipeline {
            pipeline.cancel();
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
