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
                    // Auto-scroll if user is viewing the bottom
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
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
                    if !content.is_empty() {
                        self.add_message("assistant", &content);
                    }
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
                    // Auto-scroll if user is viewing the bottom
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
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
                    tracing::debug!(tool = tool_name, call_id = %call_id, "Starting tool execution");
                    self.add_tool_call_message(&tool_name, &call_id, "executing...");
                    // Auto-scroll if user is viewing the bottom
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::ToolCallComplete { tool_name, call_id, result } => {
                    tracing::debug!(tool = tool_name, call_id = %call_id, "Tool execution complete");
                    self.add_tool_call_message(&tool_name, &call_id, &result);
                    // Auto-scroll if user is viewing the bottom
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::ToolCallError { tool_name, call_id, error } => {
                    tracing::error!(tool = tool_name, call_id = %call_id, error = %error, "Tool execution error");
                    self.add_tool_error_message(&tool_name, &call_id, &error);
                    // Auto-scroll if user is viewing the bottom
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                // Thinking output events (e.g. Claude-style reasoning)
                AppEvent::StreamThinkingChunk { content } => {
                    self.chat.current_thinking.push_str(&content);
                }
                AppEvent::StreamThinkingComplete { content } => {
                    tracing::info!("Thinking complete, content_len={}", content.len());
                    // Auto-scroll if user is viewing the bottom
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                // Agent pipeline events
                AppEvent::AgentPlanGenerated { plan_id, task_count, user_request, task_descriptions } => {
                    tracing::info!(plan_id = %plan_id, task_count, "Agent plan generated");
                    // Shorten plan ID for display (first 8 chars)
                    let plan_short = plan_id.chars().take(8).collect::<String>();
                    self.add_message("system", &format!(
                        "📋 **Plan created** (`{}`): {}", plan_short, user_request
                    ));
                    self.chat.pipeline.active = true;
                    self.chat.pipeline.plan_id = plan_id.clone();
                    self.chat.pipeline.iteration = 0;
                    // Create task entries with proper IDs and descriptions
                    self.chat.pipeline.tasks = task_descriptions.iter().enumerate()
                        .map(|(i, desc)| crate::ui::state::PipelineTaskEntry {
                            id: format!("T{}", i + 1),
                            description: desc.clone(),
                            status: crate::ui::state::PipelineTaskStatus::Pending,
                            agent_type: "general".to_string(),
                        })
                        .collect();
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::AgentTaskStarted { task_id, task_description, agent_type } => {
                    tracing::info!(task_id = %task_id, agent_type = %agent_type, "Agent task started");
                    self.add_message("system", &format!(
                        "▶️ **Running**: {}", task_description
                    ));
                    // Update pipeline task status
                    for task in &mut self.chat.pipeline.tasks {
                        if task.id == task_id {
                            task.status = crate::ui::state::PipelineTaskStatus::Running;
                            task.agent_type = agent_type.clone();
                            break;
                        }
                    }
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::AgentTaskCompleted { task_id, status, duration_ms } => {
                    tracing::info!(task_id = %task_id, status = %status, duration_ms, "Agent task completed");
                    self.add_message("system", &format!(
                        "✅ **Completed**: [{}] — {} ({:.1}s)", task_id, status, duration_ms as f64 / 1000.0
                    ));
                    // Update pipeline task status
                    for task in &mut self.chat.pipeline.tasks {
                        if task.id == task_id {
                            task.status = if status == "success" {
                                crate::ui::state::PipelineTaskStatus::Completed
                            } else {
                                crate::ui::state::PipelineTaskStatus::Failed
                            };
                            break;
                        }
                    }
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::AgentFeedbackLoop { iteration, action } => {
                    tracing::info!(iteration, action = %action, "Agent feedback loop");
                    self.add_message("system", &format!(
                        "🔄 **Round {}**: {}", iteration, action
                    ));
                    self.chat.pipeline.iteration = iteration;
                    self.chat.pipeline.feedback_state = action.clone();
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::AgentPipelineComplete { result_count, final_output } => {
                    tracing::info!(result_count, "Agent pipeline complete");
                    self.add_message("system", &format!(
                        "🎉 **Pipeline complete**: {} results returned", result_count
                    ));
                    if !final_output.is_empty() {
                        self.add_message("assistant", &format!(
                            "## Pipeline Output\n\n{}", final_output
                        ));
                    }
                    // Mark all remaining pending tasks as skipped
                    for task in &mut self.chat.pipeline.tasks {
                        if task.status == crate::ui::state::PipelineTaskStatus::Pending {
                            task.status = crate::ui::state::PipelineTaskStatus::Completed;
                        }
                    }
                    self.stop_streaming();
                    self.chat.is_pipeline_running = false;
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::AgentPipelineError { error } => {
                    tracing::error!(error = %error, "Agent pipeline error");
                    self.add_message("system", &format!("❌ **Pipeline error**: {}", error));
                    self.chat.status = crate::types::AppStatus::Error(error);
                    self.stop_streaming();
                }
                AppEvent::AgentPipelineCancelled => {
                    tracing::info!("Agent pipeline cancelled");
                    self.chat.pipeline.cancelled = true;
                    self.stop_streaming();
                    self.chat.is_pipeline_running = false;
                }
                AppEvent::AgentToolError { tool_name, task_id, error } => {
                    tracing::warn!(tool_name = %tool_name, task_id = %task_id, error = %error, "Agent tool error");
                    self.add_message("system", &format!(
                        "⚠️ **Tool error** in [{}]: tool='{}' — {}", task_id, tool_name, error
                    ));
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                // Agent engine events
                AppEvent::AgentEngineComplete { response } => {
                    tracing::info!(response_len = response.len(), "Agent engine complete");
                    self.add_message("system", "✅ **Agent execution complete**");
                    if !response.is_empty() {
                        self.add_message("assistant", &response);
                    }
                    self.stop_streaming();
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
                AppEvent::AgentEngineError { error } => {
                    tracing::error!(error = %error, "Agent engine error");
                    self.add_message("system", &format!("❌ **Agent error**: {}", error));
                    self.chat.status = AppStatus::Error(error);
                    self.stop_streaming();
                }
                AppEvent::AgentEngineStopped => {
                    tracing::info!("Agent engine stopped");
                    self.stop_streaming();
                    // Don't reset chain cancelled state here — it was set by stop_generation
                }
                // Agent chain events — delegate to chain panel processor
                AppEvent::AgentChainStarted { .. }
                | AppEvent::AgentChainCompleted { .. }
                | AppEvent::AgentChainError { .. }
                | AppEvent::AgentChainCancelled { .. }
                | AppEvent::AgentChainComplete { .. }
                | AppEvent::AgentChainStopped
                | AppEvent::NCtxUpdated { .. } => {
                    self.process_chain_event(&event);
                    if self.chat.at_bottom {
                        self.chat.scroll_to_bottom_requested = true;
                    }
                }
            }
        }
    }

    /// Try to prettify a JSON string; returns the prettified string or the original if not valid JSON.
    fn prettify_json(result: &str) -> String {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(result) {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| result.to_string())
        } else {
            result.to_string()
        }
    }

    /// Add a tool call message to the chat display.
    fn add_tool_call_message(&mut self, tool_name: &str, call_id: &str, result: &str) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        // Store as simple structured format: "header|call_id|result_json"
        let header = ChatApp::tool_call_header(tool_name, result);
        let content = format!("{}||{}||{}", header, call_id, result);
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

    /// Add a tool error message to the chat display with error styling.
    fn add_tool_error_message(&mut self, tool_name: &str, call_id: &str, error: &str) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        let formatted_error = Self::prettify_json(error);
        let content = format!("🔴 **{}** ({})\n```\nError: {}\n```", tool_name, call_id, formatted_error);
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
            self.sessions.save_failure_message = Some("Save failed — will retry on next message".to_string());
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
            self.sessions.save_failure_message = Some("Save failed — will retry on next message".to_string());
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
            let ts = chrono::Local::now().format("%H:%M:%S").to_string();
            self.chat.messages = session.messages.iter().map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                timestamp: if m.timestamp.is_empty() { ts.clone() } else { m.timestamp.clone() },
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
        self.chat.is_pipeline_running = false;
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
