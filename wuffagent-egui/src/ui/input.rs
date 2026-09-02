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
                // Send on Ctrl+Enter (allowed while generating — queues the
                // message to run after the current task finishes).
                let modifiers = ui.ctx().input(|i| i.modifiers);
                if response.has_focus()
                    && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter))
                    && modifiers.ctrl
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

                // Send is always available: while the AI is working it queues
                // the message for the next turn, otherwise it starts a run.
                let send_btn = egui::Button::new("Send")
                    .fill(theme.primary)
                    .rounding(6.0)
                    .min_size(egui::vec2(60.0, 28.0));
                if ui.add(send_btn).clicked() {
                    let input = self.chat.input_text.trim().to_string();
                    self.handle_send_input(&input);
                }
                if self.chat.is_generating {
                    ui.add_space(6.0);
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

        if self.chat.is_generating {
            // AI is still working: display the message immediately and queue it
            // to run as the next turn once the current run (and any earlier
            // queued messages) finishes.
            let image = self.chat.pending_image.take();
            self.chat.messages.push(crate::types::ChatMessage {
                kind: MessageKind::Normal,
                role: "user".to_string(),
                content: input.to_string(),
                timestamp: crate::types::format_timestamp(),
                image: image.as_ref().map(|_| String::new()),
            });
            self.chat.input_text.clear();
            self.queued_messages.push(super::state::QueuedMessage {
                text: input.to_string(),
                image,
                agent_prompt: self.resolve_agent_prompt(),
                tool_policy: self.resolve_tool_policy(),
            });
            self.chat.show_notification(
                &format!("Queued — will run after the current task ({} waiting)", self.queued_messages.len()),
                true,
            );
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

        let image = self.chat.pending_image.take();
        self.start_pipeline(&input, image, self.resolve_agent_prompt(), self.resolve_tool_policy(), false);
    }

    /// Resolve the selected agent profile's tool policy (allowed_tools +
    /// shell config) for the current selection. None/"Auto" or a profile not
    /// found yields an unrestricted policy (all tools + allow-all shell).
    fn resolve_tool_policy(&self) -> crate::client::pipeline::ChatToolPolicy {
        let policy = match self.selected_agent_index {
            Some(i) => {
                let name = self.get_agent_names().get(i).cloned().unwrap_or_default();
                self.load_agent_config(&[name.as_str()])
            }
            None => self.load_agent_config(&["general", "generalist"]),
        };
        match policy {
            Some(cfg) => crate::client::pipeline::ChatToolPolicy {
                allowed_tools: cfg.allowed_tools,
                shell_config: cfg.shell_config,
            },
            None => crate::client::pipeline::ChatToolPolicy::unrestricted(),
        }
    }

    /// Load the first matching agent profile (by `name`) from the known agents
    /// directories, handling both current `AgentConfig` and legacy `WorkerConfig`.
    fn load_agent_config(&self, names: &[&str]) -> Option<crate::agents::config::AgentConfig> {
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
                                return Some(cfg);
                            }
                        } else if let Ok(cfg) = serde_json::from_str::<crate::agents::config::WorkerConfig>(&content) {
                            if names.iter().any(|n| cfg.name == *n) {
                                let allowed_tools = cfg.allowed_tools.clone();
                                let shell_config = cfg.shell_config.clone();
                                return Some(crate::agents::config::AgentConfig {
                                    allowed_tools,
                                    shell_config,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Resolve the selected agent profile's system prompt for the current
    /// selection (None = "Auto" -> the general profile).
    fn resolve_agent_prompt(&self) -> String {
        let prompt = match self.selected_agent_index {
            Some(i) => {
                let name = self.get_agent_names().get(i).cloned().unwrap_or_default();
                self.load_agent_system_prompt(&[name.as_str()])
            }
            None => {
                self.load_agent_system_prompt(&["general", "generalist"])
            }
        };
        if prompt.is_empty() {
            tracing::warn!("No agent system prompt found - chat will run without one");
        }
        prompt
    }

    /// Start a fresh pipeline run for `text`. Used both for direct sends and
    /// for draining the queue of messages sent while the AI was working.
    ///
    /// `already_displayed` is true when the user message is already in the chat
    /// (queue drain) and false for a direct send (still needs to be pushed).
    fn start_pipeline(&mut self, text: &str, image: Option<egui::ImageSource<'static>>, agent_prompt: String, tool_policy: crate::client::pipeline::ChatToolPolicy, already_displayed: bool) {
        tracing::info!("[CHAT PATH] start_pipeline called with: {}", text);

        self.chat.is_generating = true;
        self.status = AppStatus::Generating;
        self.chat.status = AppStatus::Generating;

        // Add the user message to the chat display. Queued messages are already
        // displayed (pushed at queue time), so only direct sends need to push.
        if !already_displayed {
            self.chat.messages.push(crate::types::ChatMessage {
                kind: MessageKind::Normal,
                role: "user".to_string(),
                content: text.to_string(),
                timestamp: crate::types::format_timestamp(),
                image: image.map(|_| String::new()),
            });
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

        // The agent operates on the client's active session conversation (the
        // shared store): it appends the user turn at start of each turn and the
        // assistant/tool tail as it runs, and the UI persists the result on
        // StreamComplete. There is no separate per-agent store to reload here —
        // doing so previously hijacked the client's session pointer and leaked
        // agent context into the wrong session.

        // Create and start the chat pipeline
        let pipeline = crate::client::ChatPipeline::new(
            self.agent_engine.clone(),
            pending_tx,
            self.reasoning_effort,
        );
        // Store the pipeline for potential cancellation
        self.chat_pipeline = Some(pipeline);

        // Start the chat
        self.chat_pipeline.as_ref().unwrap().start(text, &agent_prompt, &tool_policy);
    }

    /// After a run finished, process the next queued message (if any).
    /// Called from the StreamComplete / StreamError handlers.
    pub(super) fn drain_next_queued_message(&mut self) {
        if self.queued_messages.is_empty() {
            return;
        }
        if let Some(next) = self.queued_messages.first().cloned() {
            tracing::info!(
                "[QUEUE] Starting next queued message ({} remaining): {}",
                self.queued_messages.len(),
                next.text
            );
            // The message is already displayed in the chat (pushed at queue
            // time); start_pipeline won't push it again.
            let image = next.image;
            let agent_prompt = next.agent_prompt;
            let tool_policy = next.tool_policy;
            // Remove the message we are about to run, keep the rest queued.
            self.queued_messages.remove(0);
            self.start_pipeline(&next.text, image, agent_prompt, tool_policy, true);
        }
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
                                if seen.insert(cfg.name.clone()) {
                                    names.push(cfg.name);
                                }
                            } else if let Ok(cfg) = serde_json::from_str::<crate::agents::config::WorkerConfig>(&content) {
                                if seen.insert(cfg.name.clone()) {
                                    names.push(cfg.name);
                                }
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
