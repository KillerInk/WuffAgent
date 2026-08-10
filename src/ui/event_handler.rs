use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus, ChatMessage};

impl ChatApp {
    pub(super) fn get_tool_definitions(&self) -> Vec<crate::tools::ToolDefinition> {
        self.tool_manager.get_tool_definitions()
    }

    pub(super) fn process_pending_events(&mut self) {
        // Drain all pending events first, collecting results to avoid borrow issues
        let mut events: Vec<AppEvent> = Vec::new();
        while let Ok(event) = self.pending_rx.lock().unwrap().try_recv() {
            events.push(event);
        }

        for event in events {
            match event {
                AppEvent::MessageResult { content, usage } => {
                    self.add_message("assistant", &content);
                    self.stop_streaming();
                    self.progress += 1.0;
                    let server_n_ctx = self.get_effective_n_ctx();
                    if let Some(u) = &usage {
                        self.chat.token_count = u.total_tokens;
                        self.chat.context_used = if server_n_ctx > 0 {
                            (u.total_tokens as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    } else {
                        // Fallback: estimate tokens from message content when server doesn't report usage
                        self.chat.token_count = Self::estimate_token_count(&content);
                        self.chat.context_used = if server_n_ctx > 0 {
                            (self.chat.token_count as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    }
                }
                AppEvent::MessageError { error } => {
                    self.chat.status = AppStatus::Error(error.clone());
                    self.chat.pending_error = Some(format!("Message failed: {}", error));
                    self.stop_streaming();
                }
                AppEvent::StreamChunk { content } => {
                    self.chat.current_response.push_str(&content);
                }
                AppEvent::StreamComplete { content, usage } => {
                    self.add_message("assistant", &content);
                    self.stop_streaming();
                    self.progress += 1.0;
                    let server_n_ctx = self.get_effective_n_ctx();
                    if let Some(u) = &usage {
                        self.chat.token_count = u.total_tokens;
                        self.chat.context_used = if server_n_ctx > 0 {
                            (u.total_tokens as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    } else {
                        // Fallback: estimate tokens from message content when server doesn't report usage
                        self.chat.token_count = Self::estimate_token_count(&content);
                        self.chat.context_used = if server_n_ctx > 0 {
                            (self.chat.token_count as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    }
                }
                AppEvent::StreamError { error } => {
                    self.chat.status = AppStatus::Error(error.clone());
                    self.chat.pending_error = Some(format!("Stream failed: {}", error));
                    self.stop_streaming();
                }
                AppEvent::ToolCallWarning { tool_name, message } => {
                    tracing::warn!(tool = tool_name, message = %message, "Tool call warning");
                    // Show as a pending warning (similar to error but non-fatal)
                    self.chat.pending_error = Some(format!("[{}] {}", tool_name, message));
                }
                AppEvent::ToolCallStart { tool_name, call_id } => {
                    tracing::info!(tool = tool_name, call_id = %call_id, "Starting tool execution");
                    self.add_tool_call_message(&tool_name, &call_id, "executing...");
                }
                AppEvent::ToolCallComplete { tool_name, call_id, result } => {
                    tracing::info!(tool = tool_name, call_id = %call_id, "Tool execution complete");
                    self.add_tool_call_message(&tool_name, &call_id, &result);
                }
                AppEvent::ToolCallError { tool_name, call_id, error } => {
                    tracing::error!(tool = tool_name, call_id = %call_id, error = %error, "Tool execution error");
                    self.add_tool_call_message(&tool_name, &call_id, &format!("Error: {}", error));
                }
            }
        }
    }

    /// Add a tool call message to the chat display.
    fn add_tool_call_message(&mut self, tool_name: &str, call_id: &str, result: &str) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        let content = format!("🔧 **{}** ({})\n```\n{}\n```", tool_name, call_id, result);
        self.chat.messages.push(ChatMessage {
            role: "tool".to_string(),
            content,
            timestamp: timestamp.clone(),
            image: None,
        });
        // Truncate if too many messages
        if self.chat.messages.len() > self.sessions.max_display_messages {
            self.chat.messages.drain(..self.chat.messages.len() - self.sessions.max_display_messages);
        }
    }

    pub(super) fn add_message(&mut self, role: &str, content: &str) {
        self.add_message_with_image(role, content, None);
    }

    pub(super) fn add_message_with_image(&mut self, role: &str, content: &str, image: Option<String>) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        self.chat.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: timestamp.clone(),
            image,
        });
        // Update the underlying session message with timestamp
        {
            let cl = self.client.lock().unwrap();
            let mut conv = cl.conversation().lock().unwrap();
            if let Some(last) = conv.last_mut() {
                if last.role == role && last.content == content {
                    last.timestamp = timestamp;
                }
            }
        }
        // Truncate if too many messages
        if self.chat.messages.len() > self.sessions.max_display_messages {
            self.chat.messages.drain(..self.chat.messages.len() - self.sessions.max_display_messages);
        }
        // Save session after adding message; retry any prior failed saves first
        let client = self.client.lock().unwrap();
        client.retry_pending_saves();
        drop(client);
        if let Err(e) = self.client.lock().unwrap().save_session() {
            self.sessions.save_failure_message = Some(format!("Save failed — will retry on next message"));
            eprintln!("Failed to save session: {}", e);
        } else {
            self.sessions.save_failure_message = None;
        }
    }

    pub(super) fn switch_session(&mut self, session_id: &str) {
        // Retry any prior failed saves before switching
        self.client.lock().unwrap().retry_pending_saves();
        // Save current session before switching
        if let Err(e) = self.client.lock().unwrap().save_session() {
            self.sessions.save_failure_message = Some(format!("Save failed — will retry on next message"));
            eprintln!("Failed to save session before switch: {}", e);
        } else {
            self.sessions.save_failure_message = None;
        }

        let mut cl = self.client.lock().unwrap();
        // Update session_id before loading
        let session_dir = cl.session_dir().clone();
        cl.set_session(Some(session_id.to_string()), session_dir);
        // Always update chat_display, even when load_session returns None (new session)
        let loaded = cl.load_session();
        if let Some(session) = loaded {
            self.chat.messages = session.messages.iter().map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                timestamp: m.timestamp.clone(),
                image: None,
            }).collect();
            cl.set_system_prompt(&session.system_prompt);
        } else {
            // New or empty session — clear the chat display
            self.chat.messages.clear();
        }
        drop(cl);
    }

    pub(super) fn start_streaming(&mut self) {
        self.chat.is_generating = true;
        self.chat.status = AppStatus::Generating;
        self.chat.current_response.clear();
    }

    pub(super) fn stop_streaming(&mut self) {
        self.chat.is_generating = false;
        self.chat.current_response.clear();
        self.chat.status = AppStatus::Ready;
    }

    /// Update the save-failure notification state each frame based on the client's flag.
    pub(super) fn update_save_failure_notification(&mut self) {
        let client = match self.client.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("Mutex poisoned when checking save failure: {:?}", e);
                return;
            }
        };
        if client.has_save_failure() && self.sessions.save_failure_message.is_none() {
            self.sessions.save_failure_message = Some("Save failed — will retry on next message".to_string());
        } else if !client.has_save_failure() {
            self.sessions.save_failure_message = None;
        }
        drop(client);
    }
}
