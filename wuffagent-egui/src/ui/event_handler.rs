use tracing;

use super::state::ChatApp;
use crate::types::{AppEvent, AppStatus};

impl ChatApp {
    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::StreamChunk { content } => {
                self.chat.stream_chunk(&content);
            }
            AppEvent::StreamComplete { content, usage } => {
                // Only commit the buffered stream — the chunks were already
                // delivered via StreamChunk events. Don't re-add the full content.
                self.chat.commit_stream();
                self.chat.is_generating = false;
                self.chat.is_streaming = false;
                self.status = AppStatus::Ready;
                if let Some(usage) = usage {
                    self.chat.token_count = usage.total_tokens as usize;
                    let n_ctx = self.get_effective_n_ctx();
                    if n_ctx > 0 {
                        self.chat.context_used = usage.total_tokens as f32 / n_ctx as f32 * 100.0;
                    }
                } else {
                    // Server doesn't send usage stats (common with some llama.cpp setups).
                    // Estimate from content length: ~4 chars per token.
                    self.chat.token_count = Self::estimate_token_count(&content) as usize;
                    let n_ctx = self.get_effective_n_ctx();
                    if n_ctx > 0 {
                        self.chat.context_used = self.chat.token_count as f32 / n_ctx as f32 * 100.0;
                    }
                }
                // Persist the session after each complete response
                if let Err(e) = self.save_session() {
                    eprintln!("Failed to save session: {}", e);
                }
            }
            AppEvent::StreamError { error } => {
                self.chat.stream_chunk(&format!("\n\nStream error: {}", error));
                self.chat.commit_stream();
                self.status = AppStatus::Error(error);
            }
            AppEvent::ToolCallWarning { tool_name, message } => {
                tracing::warn!(tool_name, message, "Tool call warning");
            }
            AppEvent::ToolCallStart { tool_name, call_id } => {
                tracing::debug!(tool_name, call_id, "Tool call started");
            }
            AppEvent::ToolCallComplete { tool_name, call_id: _, result } => {
                tracing::debug!(tool_name, result, "Tool call complete");
                self.chat.append_message("tool", &format!("Tool '{}' completed", tool_name));
                self.chat.append_message("system", &result);
            }
            AppEvent::ToolCallError { tool_name, call_id: _, error } => {
                tracing::warn!(tool_name, error, "Tool call error");
                self.chat.append_message("tool", &format!("Tool '{}' error: {}", tool_name, error));
            }
            AppEvent::StreamThinkingChunk { content } => {
                self.chat.stream_chunk(&format!("\n> {}", content));
            }
            AppEvent::StreamThinkingComplete { content: _ } => {
                self.chat.stream_chunk(&format!("\n\n[Thinking complete]"));
                self.chat.commit_stream();
            }
            AppEvent::AgentChainStarted { agent_name, depth } => {
                tracing::info!(agent_name, depth, "Agent chain started");
                self.agent_chain_state.active = true;
                self.agent_chain_state.current_agent = Some(agent_name);
                self.agent_chain_state.cancelled = false;
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
                });
            }
            AppEvent::AgentChainCancelled { agent_name } => {
                tracing::info!(agent_name, "Agent chain cancelled");
                self.agent_chain_state.cancelled = true;
            }
            AppEvent::AgentChainComplete { response, entries } => {
                tracing::info!("Agent chain complete");
                self.agent_chain_state.entries = entries;
                self.chat.append_message("assistant", &response);
            }
            AppEvent::AgentEngineComplete { response } => {
                tracing::info!("Agent engine complete");
                self.chat.append_message("assistant", &response);
            }
            AppEvent::AgentEngineError { error } => {
                tracing::error!(error, "Agent engine error");
                self.chat.append_message("system", &format!("Engine error: {}", error));
            }
            AppEvent::AgentEngineStopped => {
                tracing::info!("Agent engine stopped");
                self.chat.is_generating = false;
                self.chat.is_streaming = false;
                self.chat.streaming = false;
                self.chat.is_pipeline_running = false;
                self.status = AppStatus::Ready;
            }
            AppEvent::NCtxUpdated { n_ctx } => {
                tracing::info!(n_ctx, "n_ctx updated");
            }
            _ => {}
        }
    }
}
