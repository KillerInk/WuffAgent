use tracing;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus, MessageKind};

impl ChatApp {
    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::StreamChunk { content } => {
                self.chat.stream_chunk(&content);
            }
            AppEvent::StreamRoundComplete { content: _, usage } => {
                // Intermediate tool round: commit the round's text, keep generating
                // so the next round's chunks keep rendering live.
                self.chat.commit_stream();
                // Gauge: prefer the server's exact usage (input+output tokens of
                // this round); fall back to the exact char counter of the
                // stored conversation when the backend omits usage.
                if let Some(u) = &usage {
                    self.chat.token_count = u.total_tokens as usize;
                    let n_ctx = self.get_effective_n_ctx();
                    if n_ctx > 0 {
                        self.chat.context_used = u.total_tokens as f32 / n_ctx as f32 * 100.0;
                    }
                } else {
                    self.refresh_token_gauge();
                }
            }
            AppEvent::StreamComplete { content, usage } => {
                // Fallback for backends that never sent StreamChunks
                if self.chat.stream_buffer.is_empty() && !content.trim().is_empty() {
                    self.chat.append_message("assistant", &content);
                } else {
                    self.chat.commit_stream();
                }
                self.chat.is_generating = false;
                self.chat.current_thinking.clear();
                self.status = AppStatus::Ready;
                self.chat.status = AppStatus::Ready;
                if let Some(usage) = usage {
                    self.chat.token_count = usage.total_tokens as usize;
                    let n_ctx = self.get_effective_n_ctx();
                    if n_ctx > 0 {
                        self.chat.context_used = usage.total_tokens as f32 / n_ctx as f32 * 100.0;
                    }
                } else {
                    // Server doesn't send usage stats (common with some llama.cpp
                    // setups): use the exact char counter of the stored
                    // conversation to reflect the growing context.
                    self.refresh_token_gauge();
                }
                // Persist the session after each complete response
                if let Err(e) = self.save_session() {
                    tracing::warn!("Failed to save session: {}", e);
                }
                // Start the next queued message (sent while this run was active).
                self.drain_next_queued_message();
            }
            AppEvent::StreamError { error } => {
                let cancelled = error == "Cancelled";
                self.chat.stream_chunk(&format!("\n\nStream error: {}", error));
                self.chat.commit_stream();
                self.chat.is_generating = false;
                self.status = AppStatus::Error(error.clone());
                self.chat.status = AppStatus::Error(error);
                // A failed run (including a user stop surfacing as "Cancelled")
                // never emits StreamComplete, so persist the session here. The
                // agent writes the user turn and each assistant/tool round into
                // the shared conversation store as it runs; saving captures
                // whatever completed so the turn is not lost on reload.
                if let Err(e) = self.save_session() {
                    tracing::warn!("Failed to save session after error: {}", e);
                }
                // Keep the queue alive: the failed turn is retried as the next
                // turn after an earlier queued message, if any remain. An
                // explicit Stop surfaces as error "Cancelled" — don't resume
                // in that case.
                if !cancelled {
                    self.drain_next_queued_message();
                }
            }
            AppEvent::ToolCallWarning { tool_name, message } => {
                tracing::warn!(tool_name, message, "Tool call warning");
            }
            AppEvent::ToolCallStart { tool_name, call_id } => {
                tracing::debug!(tool_name, call_id, "Tool call started");
            }
            AppEvent::ToolCallComplete { tool_name, call_id, result } => {
                tracing::debug!(tool_name, result, "Tool call complete");
                let header = crate::types::tool_call_header(&tool_name, &result);
                self.chat.push_message(
                    MessageKind::Tool,
                    "tool",
                    &format!("{}||{}||{}", header, call_id, result),
                );
            }
            AppEvent::ToolCallError { tool_name, call_id, error } => {
                tracing::warn!(tool_name, error, "Tool call error");
                let header = format!("Tool '{}' error: {}", tool_name, error);
                self.chat.push_message(
                    MessageKind::Tool,
                    "tool",
                    &format!("{}||{}||", header, call_id),
                );
            }
            AppEvent::StreamThinkingChunk { content } => {
                tracing::trace!("UI: StreamThinkingChunk received, content_len={}", content.len());
                // Accumulate for live display only.
                self.chat.current_thinking.push_str(&content);
            }
            AppEvent::StreamThinkingComplete { content: _ } => {
                tracing::trace!("UI: StreamThinkingComplete received, current_thinking_len={}", self.chat.current_thinking.len());
                // Commit the thinking as a typed message, then clear live state.
                // Note: do NOT commit_stream() here — the round's text stays in
                // stream_buffer and is committed by RoundComplete/StreamComplete,
                // which keeps ordering correct (thinking first, text after) and
                // prevents the StreamComplete fallback from re-appending it.
                let thinking_text = std::mem::take(&mut self.chat.current_thinking);
                if !thinking_text.is_empty() {
                    self.chat.push_message(MessageKind::Thinking, "assistant", &thinking_text);
                }
            }
            AppEvent::AgentChainStarted { agent_name, depth } => {
                tracing::info!(agent_name, depth, "Agent chain started");
                self.agent_chain_state.active = true;
                self.agent_chain_state.current_agent = Some(agent_name);
                self.agent_chain_state.cancelled = false;
                self.show_agent_chain = true;
            }
            AppEvent::AgentChainCompleted { agent_name, result, depth } => {
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
            AppEvent::AgentChainError { agent_name, error, depth } => {
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
            AppEvent::AgentChainCancelled { agent_name } => {
                tracing::info!(agent_name, "Agent chain cancelled");
                self.agent_chain_state.cancelled = true;
            }
            AppEvent::AgentChainComplete { response, entries } => {
                tracing::info!("Agent chain complete");
                self.agent_chain_state.entries = entries;
                self.chat.push_message(MessageKind::Normal, "assistant", &response);
                self.show_agent_chain = false;
            }
            AppEvent::NCtxUpdated { n_ctx } => {
                tracing::info!(n_ctx, "n_ctx updated");
            }
            AppEvent::ImprovementSuggested { agent_name, suggestions } => {
                tracing::info!(agent_name, count = suggestions.len(), "Improvement suggestions received");
                self.improvements_panel.handle_improvement_suggested(&agent_name, suggestions);
            }
            AppEvent::SessionPaused { session_id, message_count } => {
                tracing::info!(session_id, message_count, "Session paused");
                self.chat.show_notification(&format!("Session '{}' paused ({} messages)", session_id, message_count), true);
            }
            AppEvent::SessionResumed { session_id, message_count } => {
                tracing::info!(session_id, message_count, "Session resumed");
                self.chat.show_notification(&format!("Session '{}' resumed ({} messages)", session_id, message_count), true);
                // Reload messages from the client conversation into the chat display
                self.chat.reload_messages_from_client();
            }
            _ => {}
        }
    }
}
