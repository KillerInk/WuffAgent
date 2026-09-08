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

        // Snapshot the selected session's display values up front so we don't
        // hold an immutable borrow of `self` across the self-mutating UI closures.
        let (has_session, has_image, is_generating, input_len) =
            self.selected_session_id
                .as_ref()
                .and_then(|sid| self.session_store.get(sid))
                .map(|r| {
                    (
                        true,
                        r.chat_state.pending_image.is_some(),
                        r.chat_state.is_generating,
                        r.chat_state.input_text.len(),
                    )
                })
                .unwrap_or((false, false, false, 0));

        if has_session {
            // Validate input length
            const MAX_MESSAGE_LENGTH: usize = 4000;
            if input_len > MAX_MESSAGE_LENGTH {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        theme.error,
                        format!("Message too long (max {} characters, current: {})", MAX_MESSAGE_LENGTH, input_len),
                    );
                });
            }

            // Image preview area
            if has_image {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("📷 Image attached").size(11.0).color(theme.text_secondary));
                });
            }

            // Input area: text field + button row below
            let input_width = ui.available_width();
            ui.vertical(|ui| {
                // Text input — constrained width, multiline
                ui.scope(|ui| {
                    ui.set_max_width(input_width);
                    // We can't bind to chat_state.input_text directly due to borrowing,
                    // so we use a local variable and sync it back after
                    let mut local_text = self.input_text_snapshot();
                    let text_edit = egui::TextEdit::multiline(&mut local_text)
                        .hint_text("Type a message...")
                        .desired_width(f32::INFINITY);
                    let response = ui.add(text_edit);
                    // Send on Ctrl+Enter (allowed while generating — queues the
                    // message to run after the current task finishes).
                    let modifiers = ui.ctx().input(|i| i.modifiers);
                    if response.has_focus()
                        && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter))
                        && modifiers.ctrl
                        && !local_text.trim().is_empty()
                    {
                        let input = local_text.trim().to_string();
                        self.handle_send_input(&input);
                    }
                    // Sync back to chat_state
                    if let Some(sid) = &self.selected_session_id {
                        if let Some(runtime) = self.session_store.get_mut(sid) {
                            runtime.chat_state.input_text = local_text;
                        }
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
                                    // Apply to the selected session's client (clone sid
                                    // so we don't hold an immutable borrow across).
                                    let sid = self.selected_session_id.clone();
                                    if let Some(sid) = sid {
                                        if let Some(runtime) = self.session_store.get_mut(&sid) {
                                            runtime.client.set_reasoning_effort(self.reasoning_effort);
                                        }
                                    }
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
                        let input = self.input_text_snapshot().trim().to_string();
                        if !input.is_empty() {
                            self.handle_send_input(&input);
                        }
                    }
                    if is_generating {
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
        } else {
            // No session selected — show empty input
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Select or create a session to start chatting").size(12.0).color(theme.text_secondary));
            });
        }

        // Handle image drop - simplified
        let _drop_zone = ui.allocate_space(egui::Vec2::new(ui.available_width(), 10.0));
    }

    fn handle_send_input(&mut self, input: &str) {
        // Get the selected session
        let sid = match self.selected_session_id.clone() {
            Some(sid) => sid,
            None => return,
        };

        if let Err(e) = self.validate_input(input) {
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                runtime.chat_state.pending_error = Some(e);
            }
            return;
        }

        if let Some(runtime) = self.session_store.get(&sid) {
            if runtime.chat_state.is_generating {
                // AI is still working: display the message immediately and queue it
                // to run as the next turn once the current run (and any earlier
                // queued messages) finishes.
                // Resolve the agent prompt/policy first — these borrow `self`
                // and can't be called while `cs` holds a mutable borrow of the store.
                let agent_prompt = self.resolve_agent_prompt();
                let tool_policy = self.resolve_tool_policy();
                if let Some(cs) = self.session_store.get_mut(&sid) {
                    cs.chat_state.messages.push(crate::types::ChatMessage {
                        kind: MessageKind::Normal,
                        role: "user".to_string(),
                        content: input.to_string(),
                        timestamp: crate::types::format_timestamp(),
                        image: cs.chat_state.pending_image.as_ref().map(|_| String::new()),
                    });
                    cs.chat_state.input_text.clear();
                    cs.chat_state.queued_messages.push(super::state::QueuedMessage {
                        text: input.to_string(),
                        image: cs.chat_state.pending_image.take(),
                        agent_prompt,
                        tool_policy,
                    });
                    cs.chat_state.show_notification(
                        &format!("Queued — will run after the current task ({} waiting)", cs.chat_state.queued_messages.len()),
                        true,
                    );
                }
            } else {
                self.send_message_to_session(&sid);
            }
        }
    }

    /// Snapshot the selected session's input text (empty string if no session).
    fn input_text_snapshot(&self) -> String {
        self.selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .map(|r| r.chat_state.input_text.clone())
            .unwrap_or_default()
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

    pub(super) fn send_message_to_session(&mut self, sid: &str) {
        let input = match self.session_store.get(sid) {
            Some(r) => r.chat_state.input_text.trim().to_string(),
            None => return,
        };
        if let Err(e) = self.validate_input(&input) {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                runtime.chat_state.pending_error = Some(e);
            }
            return;
        }

        let image = if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.pending_image.take()
        } else {
            None
        };
        self.start_pipeline_for_session(sid, &input, image, self.resolve_agent_prompt(), self.resolve_tool_policy(), false);
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
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| self.config.file_path.clone());
        let dirs = vec![agents_dir];

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

    /// Start a fresh pipeline run for `text` in the given session.
    fn start_pipeline_for_session(&mut self, sid: &str, text: &str, image: Option<egui::ImageSource<'static>>, agent_prompt: String, tool_policy: crate::client::pipeline::ChatToolPolicy, already_displayed: bool) {
        tracing::info!("[CHAT PATH] start_pipeline_for_session called with: {}", text);

        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.is_generating = true;
            runtime.chat_state.pending_error = None;
        }
        self.status = AppStatus::Generating;

        // Add the user message to the chat display
        if !already_displayed {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                runtime.chat_state.messages.push(crate::types::ChatMessage {
                    kind: MessageKind::Normal,
                    role: "user".to_string(),
                    content: text.to_string(),
                    timestamp: crate::types::format_timestamp(),
                    image: image.map(|_| String::new()),
                });
            }
        }

        // Cancel any in-flight pipeline in this session
        if let Some(runtime) = self.session_store.get(sid) {
            runtime.pipeline.cancel();
        }

        // Engine and client tool events both flow straight into the single
        // pending_tx the UI polls each frame - no relay task needed.
        let pending_tx = match self.pending_tx.clone() {
            Some(tx) => tx.lock().unwrap().clone(),
            None => return,
        };

        // Sync the effective n_ctx (server /props value, or local server's
        // configured value) in place on this session's client. Computed before
        // the mutable borrow; n_ctx is shared interior-mutable state, and the
        // engine's client is rebound to this same client right below, so the
        // agent loop sees the correct context budget. 0 = remote props not
        // fetched yet -> trim is skipped (safe).
        let effective_n_ctx = self.get_effective_n_ctx();
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.client.set_n_ctx(effective_n_ctx);
            // Rebind the per-session engine to this session's client so the
            // agent chat loop runs against THIS session's isolated conversation
            // store — not the shared bootstrap engine's client, which would be
            // mutated by every session in parallel (a cross-session data race).
            let new_engine = runtime.engine.clone().with_client(runtime.client.clone());
            runtime.engine = new_engine;
            let pipeline = crate::client::ChatPipeline::new(
                Arc::new(runtime.engine.clone()),
                pending_tx,
                self.reasoning_effort,
                sid.to_string(),
            );
            runtime.pipeline = pipeline;
            // Start the chat
            runtime.pipeline.start(text, &agent_prompt, &tool_policy);
        }
    }

    /// After a run finished, process the next queued message (if any).
    /// Called from the StreamComplete / StreamError handlers with the session id.
    pub(super) fn drain_next_queued_message(&mut self, sid: &str) {
        // Pop the next queued message (if any) for this session.
        let next = if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.queued_messages.first().cloned()
        } else {
            None
        };

        let next = match next {
            Some(next) => next,
            None => return,
        };

        tracing::info!(
            "[QUEUE] Starting next queued message: {}",
            next.text
        );
        // The message is already displayed in the chat (pushed at queue time);
        // start_pipeline_for_session won't push it again. Remove it from the queue
        // and hand off — `start_pipeline_for_session` re-borrows the store, so the
        // mutable borrow above must have ended (it has, via `cloned()`).
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.queued_messages.remove(0);
        }
        self.start_pipeline_for_session(sid, &next.text, next.image, next.agent_prompt, next.tool_policy, true);
    }

    /// Defensive sweep (called each frame after event processing): for any
    /// session that is still flagged `is_generating` while its pipeline task is
    /// already finished, clear the generating state and commit any partial
    /// stream.
    ///
    /// This catches the cases where a terminal event never arrives — most
    /// commonly when a new `start_pipeline_for_session` call aborts a task that
    /// was in the middle of emitting its final event, or when the task panics
    /// — so the chat-area spinner can no longer spin forever.
    pub(super) fn sweep_finished_pipelines(&mut self) {
        // Collect the ids that need finalizing (we can't mutate the store while
        // iterating it).
        let stuck: Vec<String> = self
            .session_store
            .iter()
            .filter(|(_, rt)| rt.chat_state.is_generating && rt.pipeline.task_done())
            .map(|(id, _)| id.clone())
            .collect();
        if stuck.is_empty() {
            return;
        }
        for sid in stuck {
            tracing::warn!(
                "[CHAT PATH] pipeline finished without a terminal event for session {} - clearing generating state",
                sid
            );
            let n_ctx = self.get_effective_n_ctx();
            let is_selected = self.selected_session_id.as_deref() == Some(sid.as_str());
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                runtime.chat_state.commit_stream();
                runtime.chat_state.is_generating = false;
                runtime.chat_state.current_thinking.clear();
                runtime.chat_state.status = AppStatus::Ready;
                runtime.refresh_token_gauge(n_ctx);
            }
            if is_selected {
                self.status = AppStatus::Ready;
            }
            // Persist whatever completed so the turn is not lost on reload.
            if let Err(e) = self.save_session_for(&sid) {
                tracing::warn!("Failed to save session after sweep: {}", e);
            }
            // A run that ended without an error should still let the queue flow.
            self.drain_next_queued_message(&sid);
        }
    }

    /// Load the system prompt of the first matching agent profile.
    /// Searches the same agents directories as `get_agent_names` and
    /// matches the profile's `name` field (not the file name).
    /// Returns an empty string when no candidate profile exists.
    fn load_agent_system_prompt(&self, names: &[&str]) -> String {
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| self.config.file_path.clone());
        let dirs = vec![agents_dir];

        let mut prompt = String::new();
        for dir in &dirs {
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
                    "\n\nYou can delegate tasks to other agents using the agent_call tool. Available agents: {}. \
                     Prefer your own tools when a task can be done in a single step — \
                     delegate only when the sub-task needs another agent's specialization, \
                     since each delegation spawns a full sub-conversation and is more \
                     expensive than a direct tool call.",
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
                                if cfg.enabled && seen.insert(cfg.name.clone()) {
                                    names.push(cfg.name);
                                }
                            } else if let Ok(cfg) = serde_json::from_str::<crate::agents::config::WorkerConfig>(&content) {
                                if cfg.enabled && seen.insert(cfg.name.clone()) {
                                    names.push(cfg.name);
                                }
                            }
                        }
                    }
                }
            }
        };

        // Scan only the config-directory agents subdirectory
        let agents_dir = config_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| config_path.clone());
        scan_dir(agents_dir);

        names
    }

    pub(super) fn stop_generation(&mut self) {
        tracing::info!("[CANCEL] Stopping all generation");

        // Cancel the pipeline in the selected session
        if let Some(sid) = self.selected_session_id.clone() {
            if let Some(runtime) = self.session_store.get(&sid) {
                runtime.cancel();
            }
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                runtime.chat_state.is_generating = false;
                runtime.chat_state.pending_error = None;
            }
        }

        // Cancel agent engine
        self.agent_cancel_token.cancel();

        // Mark the chain as cancelled so the UI reflects it immediately
        self.agent_chain_state.cancelled = true;

        // Flush any in-progress streamed text into the display, then persist the
        // session.
        if let Some(sid) = self.selected_session_id.clone() {
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                runtime.chat_state.commit_stream();
            }
        }
        if let Err(e) = self.save_session() {
            tracing::warn!("Failed to save session after stop: {}", e);
        }
    }
}
