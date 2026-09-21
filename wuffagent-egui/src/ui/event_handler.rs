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
            | AppEvent::NCtxUpdated { session_id, .. }
            | AppEvent::ImprovementSuggested { session_id, .. }
            | AppEvent::AgentHandoff { session_id, .. }
            | AppEvent::RestartRequested { session_id, .. }
            => session_id.clone(),
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
                // Token tracker: a round just finished and was logged.
                self.usage_panel.mark_dirty();
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
                // Token tracker: the run finished and its final round was logged.
                self.usage_panel.mark_dirty();
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
                // explicit Stop surfaces as error "Cancelled" â€” don't resume
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
                // Note: do NOT commit_stream() here â€” the round's text stays in
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
            AppEvent::NCtxUpdated { n_ctx, session_id: _ } => {
                tracing::info!(n_ctx, "n_ctx updated");
            }
            AppEvent::AgentHandoff { from, to, task, .. } => {
                tracing::info!(from, to, "Agent handoff");
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    // The session is now owned by the target agent: follow the
                    // switch so follow-up messages (and the next queued one)
                    // run under the new profile.
                    runtime.selected_agent = Some(to.clone());
                    // Visible banner in the chat transcript.
                    runtime.chat_state.push_message(
                        MessageKind::Normal,
                        "system",
                        &format!("🔀 Handoff: {} → {} — {}", from, to, task),
                    );
                }
            }
            AppEvent::ImprovementSuggested { agent_name, suggestions, session_id: _ } => {
                tracing::info!(agent_name, count = suggestions.len(), "Improvement suggestions received");
                self.improvements_panel.handle_improvement_suggested(&agent_name, suggestions);
            }
            AppEvent::RestartRequested { reason, exe_path, .. } => {
                tracing::info!(reason, "Restart requested");
                // Persist the transcript so nothing is lost across the relaunch,
                // show a banner, then relaunch the (optionally newly built)
                // binary — the marker + auto-resume pick the work back up.
                if let Err(e) = self.save_session_for(&sid) {
                    tracing::warn!("Failed to save session before restart: {}", e);
                }
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.push_message(
                        MessageKind::Normal,
                        "system",
                        &format!("🔁 Restart: {}", reason),
                    );
                }
                self.perform_restart(reason, exe_path);
            }
        }
    }
}
