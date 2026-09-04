use tracing;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus, MessageKind};

impl ChatApp {
    pub fn handle_event(&mut self, event: AppEvent) {
        // Compute n_ctx up front (used by several arms below).
        let n_ctx = self.get_effective_n_ctx();

        // Extract sid via a helper to avoid borrow conflicts with the match.
        let sid: String = match &event {
            AppEvent::StreamChunk { session_id, .. }
            | AppEvent::StreamRoundComplete { session_id, .. }
            | AppEvent::StreamComplete { session_id, .. }
            | AppEvent::StreamError { session_id, .. }
            | AppEvent::ToolCallWarning { session_id, .. }
            | AppEvent::ToolCallStart { session_id, .. }
            | AppEvent::ToolCallComplete { session_id, .. }
            | AppEvent::ToolCallError { session_id, .. }
            | AppEvent::StreamThinkingChunk { session_id, .. }
            | AppEvent::StreamThinkingComplete { session_id, .. }
            | AppEvent::AgentChainStarted { session_id, .. }
            | AppEvent::AgentChainCompleted { session_id, .. }
            | AppEvent::AgentChainError { session_id, .. }
            | AppEvent::AgentChainCancelled { session_id, .. }
            | AppEvent::AgentChainComplete { session_id, .. }
            | AppEvent::NCtxUpdated { session_id, .. }
            | AppEvent::ImprovementSuggested { session_id, .. }
            | AppEvent::AgentChainStopped { session_id, .. } => session_id.clone(),
        };
        match event {
            AppEvent::StreamChunk { content, .. } => {
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.stream_chunk(&content);
                }
            }
            AppEvent::StreamRoundComplete { content: _, usage, .. } => {
                // Intermediate tool round: commit the round's text, keep generating
                // so the next round's chunks keep rendering live.
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.commit_stream();
                    // Gauge: prefer the server's exact usage (input+output tokens of
                    // this round); fall back to the exact char counter of the
                    // stored conversation when the backend omits usage.
                    if let Some(u) = &usage {
                        runtime.chat_state.token_count = u.total_tokens as usize;
                        if n_ctx > 0 {
                            runtime.chat_state.context_used = u.total_tokens as f32 / n_ctx as f32 * 100.0;
                        }
                    } else {
                        runtime.refresh_token_gauge(n_ctx);
                    }
                }
            }
            AppEvent::StreamComplete { content, usage, .. } => {
                let is_selected = self.selected_session_id.as_deref() == Some(sid.as_str());
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    // Fallback for backends that never sent StreamChunks
                    if runtime.chat_state.stream_buffer.is_empty() && !content.trim().is_empty() {
                        runtime.chat_state.append_message("assistant", &content);
                    } else {
                        runtime.chat_state.commit_stream();
                    }
                    runtime.chat_state.is_generating = false;
                    runtime.chat_state.current_thinking.clear();
                    if is_selected {
                        self.status = AppStatus::Ready;
                    }
                    runtime.chat_state.status = AppStatus::Ready;
                    if let Some(usage) = usage {
                        runtime.chat_state.token_count = usage.total_tokens as usize;
                        if n_ctx > 0 {
                            runtime.chat_state.context_used = usage.total_tokens as f32 / n_ctx as f32 * 100.0;
                        }
                    } else {
                        runtime.refresh_token_gauge(n_ctx);
                    }
                }
                // Persist the session after each complete response.
                if let Err(e) = self.save_session_for(&sid) {
                    tracing::warn!("Failed to save session: {}", e);
                }
                // Start the next queued message (sent while this run was active).
                self.drain_next_queued_message(&sid);
            }
            AppEvent::StreamError { error, .. } => {
                let cancelled = error == "Cancelled";
                let is_selected = self.selected_session_id.as_deref() == Some(sid.as_str());
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.stream_chunk(&format!("\n\nStream error: {}", error));
                    runtime.chat_state.commit_stream();
                    runtime.chat_state.is_generating = false;
                    if is_selected {
                        self.status = AppStatus::Error(error.clone());
                    }
                    runtime.chat_state.status = AppStatus::Error(error);
                }
                // A failed run (including a user stop surfacing as "Cancelled")
                // never emits StreamComplete, so persist the session here. The
                // agent writes the user turn and each assistant/tool round into
                // the shared conversation store as it runs; saving captures
                // whatever completed so the turn is not lost on reload.
                if let Err(e) = self.save_session_for(&sid) {
                    tracing::warn!("Failed to save session after error: {}", e);
                }
                // Keep the queue alive: the failed turn is retried as the next
                // turn after an earlier queued message, if any remain. An
                // explicit Stop surfaces as error "Cancelled" — don't resume
                // in that case.
                if !cancelled {
                    self.drain_next_queued_message(&sid);
                }
            }
            AppEvent::ToolCallWarning { tool_name, message, .. } => {
                tracing::warn!(tool_name, message, "Tool call warning");
            }
            AppEvent::ToolCallStart { tool_name, call_id, .. } => {
                tracing::debug!(tool_name, call_id, "Tool call started");
            }
            AppEvent::ToolCallComplete { tool_name, call_id, result, .. } => {
                tracing::debug!(tool_name, result, "Tool call complete");
                let header = crate::types::tool_call_header(&tool_name, &result);
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.push_message(
                        MessageKind::Tool,
                        "tool",
                        &format!("{}||{}||{}", header, call_id, result),
                    );
                }
            }
            AppEvent::ToolCallError { tool_name, call_id, error, .. } => {
                tracing::warn!(tool_name, error, "Tool call error");
                let header = format!("Tool '{}' error: {}", tool_name, error);
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.push_message(
                        MessageKind::Tool,
                        "tool",
                        &format!("{}||{}||", header, call_id),
                    );
                }
            }
            AppEvent::StreamThinkingChunk { content, .. } => {
                tracing::trace!("UI: StreamThinkingChunk received, content_len={}", content.len());
                // Accumulate for live display only.
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.current_thinking.push_str(&content);
                }
            }
            AppEvent::StreamThinkingComplete { content: _, .. } => {
                tracing::trace!("UI: StreamThinkingComplete received, current_thinking_len={}", 
                    self.session_store.get(&sid).map(|r| r.chat_state.current_thinking.len()).unwrap_or(0));
                // Commit the thinking as a typed message, then clear live state.
                // Note: do NOT commit_stream() here — the round's text stays in
                // stream_buffer and is committed by RoundComplete/StreamComplete,
                // which keeps ordering correct (thinking first, text after) and
                // prevents the StreamComplete fallback from re-appending it.
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    let thinking_text = std::mem::take(&mut runtime.chat_state.current_thinking);
                    if !thinking_text.is_empty() {
                        runtime.chat_state.push_message(MessageKind::Thinking, "assistant", &thinking_text);
                    }
                }
            }
            AppEvent::AgentChainStarted { agent_name, depth, session_id: _ } => {
                tracing::info!(agent_name, depth, "Agent chain started");
                self.agent_chain_state.active = true;
                self.agent_chain_state.current_agent = Some(agent_name);
                self.agent_chain_state.cancelled = false;
                self.show_agent_chain = true;
            }
            AppEvent::AgentChainCompleted { agent_name, result, depth, session_id: _ } => {
                tracing::info!(agent_name, depth, "Agent chain completed");
                self.agent_chain_state.current_agent = None;
                if !result.is_empty() {
                    self.agent_chain_state.entries.push(crate::sessions::model::AgentChainEntry {
                        agent_name,
                        request: result.clone(),
                        result,
                        depth,
                        tool_calls: vec![],
                        completed_at: chrono::Utc::now(),
                        error: None,
                        status: crate::sessions::model::AgentChainEntryStatus::Completed,
                        checkpoint: None,
                    });
                }
            }
            AppEvent::AgentChainError { agent_name, error, depth, session_id: _ } => {
                tracing::error!(agent_name, depth, error, "Agent chain error");
                self.agent_chain_state.current_agent = None;
                self.agent_chain_state.entries.push(crate::sessions::model::AgentChainEntry {
                    agent_name,
                    request: String::new(),
                    result: error.clone(),
                    depth,
                    tool_calls: vec![],
                    completed_at: chrono::Utc::now(),
                    error: Some(error),
                    status: crate::sessions::model::AgentChainEntryStatus::Failed,
                    checkpoint: None,
                });
            }
            AppEvent::AgentChainCancelled { agent_name, session_id: _ } => {
                tracing::info!(agent_name, "Agent chain cancelled");
                self.agent_chain_state.cancelled = true;
            }
            AppEvent::AgentChainComplete { response, entries, session_id: _ } => {
                tracing::info!("Agent chain complete");
                self.agent_chain_state.entries = entries;
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.push_message(MessageKind::Normal, "assistant", &response);
                }
                self.show_agent_chain = false;
            }
            AppEvent::AgentChainStopped { session_id: _ } => {
                tracing::info!("Agent chain stopped");
                self.agent_chain_state.cancelled = true;
                self.agent_chain_state.active = false;
                self.agent_chain_state.current_agent = None;
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.is_generating = false;
                }
            }
            AppEvent::NCtxUpdated { n_ctx, session_id: _ } => {
                tracing::info!(n_ctx, "n_ctx updated");
            }
            AppEvent::ImprovementSuggested { agent_name, suggestions, session_id: _ } => {
                tracing::info!(agent_name, count = suggestions.len(), "Improvement suggestions received");
                self.improvements_panel.handle_improvement_suggested(&agent_name, suggestions);
            }
        }
    }
}
