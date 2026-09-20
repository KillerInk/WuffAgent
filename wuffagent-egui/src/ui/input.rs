use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
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
        // The agent selection is per-session (SessionRuntime.selected_agent).
        // `pending_image` is cloned so the preview below can draw it without
        // holding a borrow of the store.
        let (has_session, pending_image, is_generating, input_len, has_history, selected_agent) =
            self.selected_session_id
                .as_ref()
                .and_then(|sid| self.session_store.get(sid))
                .map(|r| {
                    (
                        true,
                        r.chat_state.pending_image.clone(),
                        r.chat_state.is_generating,
                        r.chat_state.input_text.len(),
                        !r.chat_state.messages.is_empty(),
                        r.selected_agent.clone(),
                    )
                })
                .unwrap_or((false, None, false, 0, false, None));

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

            // Image preview: the image that will be attached to the next
            // message (pasted with Ctrl/Cmd+V, or added via the attach button).
            if let Some(img) = pending_image.clone() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Image attached:").size(11.0).color(theme.text_secondary));
                    ui.add(egui::Image::new(img).max_size(egui::Vec2::new(56.0, 56.0)));
                    if ui
                        .add(egui::Button::new("x").min_size(egui::vec2(18.0, 18.0)))
                        .clicked()
                    {
                        if let Some(sid) = self.selected_session_id.clone() {
                            if let Some(runtime) = self.session_store.get_mut(&sid) {
                                runtime.chat_state.pending_image = None;
                            }
                        }
                    }
                });
                ui.add_space(4.0);
            }

            // Input area: text field + button row below
            let input_width = ui.available_width();
            ui.vertical(|ui| {
                // Text input â€” constrained width, multiline
                ui.scope(|ui| {
                    ui.set_max_width(input_width);
                    // We can't bind to chat_state.input_text directly due to borrowing,
                    // so we use a local variable and sync it back after
                    let mut local_text = self.input_text_snapshot();
                    let text_edit = egui::TextEdit::multiline(&mut local_text)
                        .hint_text("Type a message...")
                        .desired_width(f32::INFINITY);
                    // Themed container matching the chat bubbles.
                    // NOTE: use `.inner` (the TextEdit's own response), not the
                    // Frame's — `has_focus()` is id-based and the Frame's
                    // response never reports the inner edit's focus.
                    let response = egui::Frame::NONE
                        .fill(theme.surface)
                        .stroke(egui::Stroke::new(1.0, theme.border))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| ui.add(text_edit))
                        .inner;
                    // Send on Ctrl+Enter (allowed while generating â€” queues the
                    // message to run after the current task finishes).
                    let modifiers = ui.ctx().input(|i| i.modifiers);
                    if response.has_focus()
                        && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter))
                        && modifiers.ctrl
                        && !local_text.trim().is_empty()
                    {
                        let input = local_text.trim().to_string();
                        if self.handle_send_input(&input) {
                            // Accepted (queued or started): clear the box so the
                            // sync-back below doesn't restore the sent text.
                            local_text.clear();
                        }
                    }
                    // Paste an image (Ctrl/Cmd+V, or Shift+Insert on Windows).
                    //
                    // egui-winit can only paste TEXT: for an image-only
                    // clipboard it logs "arboard paste error" and swallows the
                    // key press entirely (no Event::Paste, no Event::Key for
                    // the press). The RELEASE of the V key still reaches us,
                    // so we detect the shortcut there and attach the clipboard
                    // image ourselves (no-op when the clipboard holds text -
                    // egui's own paste already ran in that case).
                    let paste_attempted = ui.ctx().input(|i| {
                        (i.key_released(egui::Key::V) && i.modifiers.command)
                            || (cfg!(target_os = "windows")
                                && i.key_released(egui::Key::Insert)
                                && i.modifiers.shift)
                    });
                    if response.has_focus() && paste_attempted {
                        self.paste_image_from_clipboard();
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
                    // Per-session agent selector (bound to the current session's
                    // SessionRuntime.selected_agent, not a global index).
                    let agent_names = self.get_agent_names();
                    let selected_label = selected_agent
                        .clone()
                        .unwrap_or_else(|| "Auto".to_string());
                    let mut next_agent: Option<String> = selected_agent.clone();
                    egui::ComboBox::from_id_salt("agent_selector")
                        .width(120.0)
                        .selected_text(selected_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut next_agent, None, "Auto");
                            for name in agent_names.iter() {
                                ui.selectable_value(&mut next_agent, Some(name.clone()), name);
                            }
                            if agent_names.is_empty() {
                                ui.label(egui::RichText::new("No agents found").size(10.0).color(theme.text_secondary));
                            }
                        });
                    if let Some(next) = next_agent {
                        if let Some(sid) = self.selected_session_id.clone() {
                            if let Some(runtime) = self.session_store.get_mut(&sid) {
                                runtime.selected_agent = Some(next);
                            }
                        }
                    } else if let Some(sid) = self.selected_session_id.clone() {
                        if let Some(runtime) = self.session_store.get_mut(&sid) {
                            runtime.selected_agent = None;
                        }
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

                    // Attach an image to the next message: clipboard first
                    // (the screenshot flow), file picker as fallback.
                    let attach_btn = egui::Button::new(egui::RichText::new("IMG").size(11.0))
                        .fill(theme.surface_light)
                        .corner_radius(8)
                        .min_size(egui::vec2(36.0, 28.0));
                    let attach_tooltip = if pending_image.is_some() {
                        "Image attached to the next message - click to replace"
                    } else {
                        "Attach image (from clipboard, or pick a file)"
                    };
                    if ui.add(attach_btn).on_hover_text(attach_tooltip).clicked() {
                        self.attach_image_from_clipboard_or_file();
                    }
                    ui.add_space(6.0);

                    // Send is always available: while the AI is working it queues
                    // the message for the next turn, otherwise it starts a run.
                    let send_btn = egui::Button::new(
                        egui::RichText::new("Send").color(egui::Color32::WHITE),
                    )
                    .fill(theme.primary)
                    .corner_radius(8)
                    .min_size(egui::vec2(60.0, 28.0));
                    if ui.add(send_btn).clicked() {
                        let input = self.input_text_snapshot().trim().to_string();
                        if !input.is_empty() && self.handle_send_input(&input) {
                            // Accepted (queued or started): clear the input box.
                            if let Some(sid) = self.selected_session_id.clone() {
                                if let Some(runtime) = self.session_store.get_mut(&sid) {
                                    runtime.chat_state.input_text.clear();
                                }
                            }
                        }
                    }
                    if is_generating {
                        ui.add_space(6.0);
                        let stop_btn = egui::Button::new(
                            egui::RichText::new("Stop").color(egui::Color32::WHITE),
                        )
                        .fill(theme.error)
                        .corner_radius(8)
                        .min_size(egui::vec2(60.0, 28.0));
                        if ui.add(stop_btn).clicked() {
                            self.stop_generation();
                        }
                    } else if has_history {
                        // Session is idle with a preserved conversation: offer to
                        // continue the agent run on that conversation.
                        ui.add_space(6.0);
                        let continue_btn = egui::Button::new("▶ Continue")
                            .fill(theme.surface_light)
                            .corner_radius(6)
                            .min_size(egui::vec2(88.0, 28.0));
                        if ui.add(continue_btn).clicked() {
                            self.continue_generation();
                        }
                    }
                });
            });
        } else {
            // No session selected â€” show empty input
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Select or create a session to start chatting").size(12.0).color(theme.text_secondary));
            });
        }

        // Handle image drop - simplified
        let _drop_zone = ui.allocate_space(egui::Vec2::new(ui.available_width(), 10.0));
    }

    /// Attempt to send `input` as a new user message: immediately when the
    /// session is idle, or queued behind the running turn while generating.
    /// Returns `true` when the message was accepted (so callers can clear
    /// the input box), `false` when validation failed or nothing happened.
    fn handle_send_input(&mut self, input: &str) -> bool {
        // Get the selected session
        let sid = match self.selected_session_id.clone() {
            Some(sid) => sid,
            None => return false,
        };

        if let Err(e) = self.validate_input(input) {
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                runtime.chat_state.pending_error = Some(e);
            }
            return false;
        }

        if let Some(runtime) = self.session_store.get(&sid) {
            if runtime.chat_state.is_generating {
                // AI is still working: display the message immediately and queue it
                // to run as the next turn once the current run (and any earlier
                // queued messages) finishes.
                // Resolve the agent prompt/policy first â€” these borrow `self`
                // and can't be called while `cs` holds a mutable borrow of the store.
                let agent = runtime.selected_agent.clone().unwrap_or_default();
                let agent_prompt = self.resolve_agent_prompt(&agent);
                let tool_policy = self.resolve_tool_policy(&agent);
                if let Some(cs) = self.session_store.get_mut(&sid) {
                    cs.chat_state.messages.push(crate::types::ChatMessage {
                        kind: MessageKind::Normal,
                        role: "user".to_string(),
                        content: input.to_string(),
                        timestamp: crate::types::format_timestamp(),
                        image: pending_image_b64(cs.chat_state.pending_image.as_ref()),
                    });
                    cs.chat_state.input_text.clear();
                    cs.chat_state.queued_messages.push(super::state::QueuedMessage {
                        text: input.to_string(),
                        image: cs.chat_state.pending_image.take(),
                        agent_prompt,
                        tool_policy,
                    });
                    cs.chat_state.show_notification(
                        &format!("Queued â€” will run after the current task ({} waiting)", cs.chat_state.queued_messages.len()),
                        true,
                    );
                }
            } else {
                self.send_message_to_session(&sid);
            }
        }
        true
    }

    /// Paste an image from the system clipboard into the selected session's
    /// pending-image slot. No-op when the clipboard holds no image (text is
    /// pasted by egui itself; an empty clipboard simply does nothing).
    fn paste_image_from_clipboard(&mut self) {
        if self.selected_session_id.is_none() {
            return;
        }
        if let Some(rgba) = clipboard_image_pixels() {
            self.attach_rgba(rgba, "clipboard");
        }
    }

    /// "Attach image" button: try the system clipboard first (the screenshot
    /// flow), then fall back to a file picker for common image formats.
    fn attach_image_from_clipboard_or_file(&mut self) {
        if self.selected_session_id.is_none() {
            return;
        }
        if let Some(rgba) = clipboard_image_pixels() {
            if self.attach_rgba(rgba, "clipboard") {
                return;
            }
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
            .pick_file()
        {
            match image_pixels_from_file(&path) {
                Some(rgba) => {
                    self.attach_rgba(rgba, "file");
                }
                None => self.notify_chat(
                    &format!(
                        "Could not read image file: {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                    false,
                ),
            }
        }
    }

    /// Store RGBA8 pixels as the session's pending image (re-encoded as PNG).
    /// Replaces any previously attached image (one image per message).
    /// Returns true on success.
    fn attach_rgba(&mut self, rgba: (u32, u32, Vec<u8>), source: &str) -> bool {
        let sid = match self.selected_session_id.clone() {
            Some(sid) => sid,
            None => return false,
        };
        let (w, h, bytes) = rgba;
        let expected = w.saturating_mul(h).saturating_mul(4) as usize;
        if w == 0 || h == 0 || bytes.len() != expected {
            tracing::warn!(
                "[IMAGE] bad pixel data from {}: {}x{} ({} bytes, expected {})",
                source,
                w,
                h,
                bytes.len(),
                expected
            );
            return false;
        }
        let png = match png_bytes_from_rgba(w, h, &bytes) {
            Some(p) => p,
            None => {
                tracing::warn!("[IMAGE] PNG encoding failed for {} image", source);
                return false;
            }
        };
        let png_len = png.len();
        if let Some(runtime) = self.session_store.get_mut(&sid) {
            // URI unique per content: egui's bytes loader keeps the FIRST
            // payload stored for a URI, so a fixed URI would show a stale
            // image whenever a different one is attached (per-session
            // pending images would also collide).
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hasher::write(&mut hasher, png.as_slice());
            let hash = std::hash::Hasher::finish(&hasher);
            runtime.chat_state.pending_image = Some(egui::ImageSource::Bytes {
                uri: format!("bytes://attached_image_{hash:016x}.png").into(),
                bytes: png.into(),
            });
        }
        tracing::info!(
            "[IMAGE] attached {}x{} image from {} ({} KB PNG)",
            w,
            h,
            source,
            png_len / 1024
        );
        true
    }

    /// Show a brief system notification in the selected session's chat area.
    fn notify_chat(&mut self, msg: &str, success: bool) {
        if let Some(sid) = self.selected_session_id.clone() {
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                runtime.chat_state.show_notification(msg, success);
            }
        }
    }

    /// Continue the current session's conversation: re-invoke the session's agent
    /// on the preserved conversation store with a "continue" turn. No checkpoint
    /// machinery â€” the pipeline simply resumes on the existing messages.
    pub(super) fn continue_generation(&mut self) {
        self.continue_generation_note("Continue from where you left off.");
    }

    /// Like [`Self::continue_generation`] but with an explicit resume note — used
    /// by the post-restart auto-resume, which carries the reason the agent gave
    /// for restarting so the (newly reloaded) agent picks the work back up.
    pub(super) fn continue_generation_note(&mut self, note: &str) {
        let sid = match self.selected_session_id.clone() {
            Some(sid) => sid,
            None => return,
        };
        if let Some(runtime) = self.session_store.get(&sid) {
            if runtime.chat_state.is_generating {
                return;
            }
        }
        let agent = self
            .selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .and_then(|r| r.selected_agent.clone())
            .unwrap_or_default();
        let agent_prompt = self.resolve_agent_prompt(&agent);
        let tool_policy = self.resolve_tool_policy(&agent);
        self.start_pipeline_for_session(&sid, note, None, agent_prompt, tool_policy, false);
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
            return Err(format!("Message too long (max {} characters)", text.len()));
        }
        Ok(())
    }

    pub(super) fn send_message_to_session(&mut self, sid: &str) {
        let (input, agent) = match self.session_store.get(sid) {
            Some(r) => (r.chat_state.input_text.trim().to_string(), r.selected_agent.clone().unwrap_or_default()),
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
        let agent_prompt = self.resolve_agent_prompt(&agent);
        let tool_policy = self.resolve_tool_policy(&agent);
        self.start_pipeline_for_session(sid, &input, image, agent_prompt, tool_policy, false);
    }

    /// Resolve the tool policy for an agent profile by name. An empty name
    /// ("Auto") or a profile not found yields an unrestricted policy (all tools
    /// + allow-all shell, no handoff).
    fn resolve_tool_policy(&self, agent_name: &str) -> crate::client::pipeline::ChatToolPolicy {
        let names: Vec<&str> = if agent_name.is_empty() {
            vec!["general", "generalist"]
        } else {
            vec![agent_name]
        };
        let policy = self.load_agent_config(&names);
        match policy {
            Some(cfg) => crate::client::pipeline::ChatToolPolicy {
                allowed_tools: cfg.allowed_tools,
                shell_config: cfg.shell_config,
                agent_name: cfg.name,
                handoff_enabled: cfg.handoff_enabled,
                handoff_targets: cfg.handoff_targets,
                restart_enabled: cfg.restart_enabled,
                reasoning_effort: cfg.reasoning_effort,
                trim_config: cfg.trim_config,
            },
            None => crate::client::pipeline::ChatToolPolicy::unrestricted(),
        }
    }

    /// The known agents directories in priority order: the config-dir
    /// `agents/` first, then the project-level `agents/` dirs (cwd, exe dir) —
    /// the same discovery set the UI agent dialog, the improvements panel (F3/F4), and the bootstrap engine use.
    pub(super) fn agents_dirs(&self) -> Vec<PathBuf> {
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| self.config.file_path.clone());
        let mut dirs = vec![agents_dir];
        if let Ok(cwd) = std::env::current_dir() {
            let d = cwd.join("agents");
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let d = exe_dir.join("agents");
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
        }
        dirs
    }

    /// Load the first matching agent profile (by `name`) from the known agents
    /// directories, handling both current `AgentConfig` and legacy `WorkerConfig`
    /// (via the core loader, which preserves every field incl. handoff settings).
    fn load_agent_config(&self, names: &[&str]) -> Option<crate::agents::config::AgentConfig> {
        let dirs = self.agents_dirs();
        for name in names {
            if let Some(cfg) = crate::agents::config::load_agent_from_dirs(&dirs, name) {
                return Some(cfg);
            }
        }
        None
    }

    /// Resolve the system prompt for an agent profile by name (empty = "Auto"
    /// -> the general profile).
    fn resolve_agent_prompt(&self, agent_name: &str) -> String {
        let names: Vec<&str> = if agent_name.is_empty() {
            vec!["general", "generalist"]
        } else {
            vec![agent_name]
        };
        let prompt = self.load_agent_system_prompt(&names);
        if prompt.is_empty() {
            tracing::warn!("No agent system prompt found - chat will run without one");
        }
        prompt
    }

    /// Start a fresh pipeline run for `text` in the given session.
    fn start_pipeline_for_session(&mut self, sid: &str, text: &str, image: Option<egui::ImageSource<'static>>, agent_prompt: String, tool_policy: crate::client::pipeline::ChatToolPolicy, already_displayed: bool) {
        tracing::info!("[CHAT PATH] start_pipeline_for_session called with: {}", text);

        // Convert the attached image (if any) into the two forms we need:
        // raw base64 for the chat display (`ChatMessage.image`) and a `data:`
        // URI for the model request (core serializes it into an OpenAI-style
        // `image_url` content part on the user message).
        let image_b64 = pending_image_b64(image.as_ref());
        let image_data_uri =
            image_b64.as_deref().map(|b64| format!("data:image/png;base64,{}", b64));

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
                    image: image_b64.clone(),
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
            // store â€” not the shared bootstrap engine's client, which would be
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
            // Start the chat (with the attached image as a data: URI, if any)
            runtime.pipeline.start(text, &agent_prompt, &tool_policy, image_data_uri.as_deref());
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
        // and hand off â€” `start_pipeline_for_session` re-borrows the store, so the
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
    /// This catches the cases where a terminal event never arrives â€” most
    /// commonly when a new `start_pipeline_for_session` call aborts a task that
    /// was in the middle of emitting its final event, or when the task panics
    /// â€” so the chat-area spinner can no longer spin forever.
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
        let dirs = self.agents_dirs();

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

        // The handoff hint is appended by `Agent::build_system_prompt`
        // when the profile has `handoff_enabled` â€” mirroring it here would
        // advertise handoffs the chat agent is not allowed to make.

        prompt
    }

    /// Return the list of agent names from all known agents directories.
    fn get_agent_names(&self) -> Vec<String> {
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

        // Scan all known agents directories (config dir first, then the
        // project-level `agents/` dirs) — same discovery set as the UI agent
        // dialog, so the selector lists the same profiles it can edit.
        for dir in self.agents_dirs() {
            scan_dir(dir);
        }

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

/// RGBA8 pixels (width, height, bytes) from the system clipboard, or `None`.
///
/// egui-winit can only paste TEXT from the clipboard: with an image-only
/// clipboard it logs "arboard paste error" and emits no event at all for the
/// key press, so image pastes are handled here instead. When the clipboard
/// also holds text, egui's own text paste already ran, so we return `None`
/// to avoid duplicating it.
fn clipboard_image_pixels() -> Option<(u32, u32, Vec<u8>)> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    if clipboard.get_text().is_ok() {
        return None;
    }
    let img = match clipboard.get_image() {
        Ok(img) => img,
        Err(e) => {
            tracing::debug!("[IMAGE] clipboard has no image: {}", e);
            return None;
        }
    };
    if img.width == 0 || img.height == 0 || img.bytes.is_empty() {
        return None;
    }
    Some((img.width as u32, img.height as u32, img.bytes.into_owned()))
}

/// Decode an image file (png/jpg/jpeg/webp/bmp/gif) into RGBA8 pixels.
fn image_pixels_from_file(path: &std::path::Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?.into_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// Encode RGBA8 pixels as PNG bytes.
fn png_bytes_from_rgba(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(buf.into_inner())
}

/// Raw base64 (STANDARD) of a pending image's PNG bytes, for the chat display
/// (`ChatMessage.image`). Only `Bytes`-based sources carry the payload.
fn pending_image_b64(source: Option<&egui::ImageSource<'static>>) -> Option<String> {
    match source? {
        egui::ImageSource::Bytes { bytes, .. } => {
            Some(base64::engine::general_purpose::STANDARD.encode(bytes.as_ref()))
        }
        _ => None,
    }
}
