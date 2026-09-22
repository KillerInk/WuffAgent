use tracing;

use super::state::ChatApp;
use wuffagent_core::types::{AppEvent, AppStatus, MessageKind};

impl ChatApp {
    pub fn handle_event(&mut self, event: AppEvent) {
        // Compute n_ctx up front (used by several arms below).
        let n_ctx = self.get_effective_n_ctx();

        // Extract sid via a helper to avoid borrow conflicts with the match.
        let sid: String = match &event {
            AppEvent::StreamChunk { session_id, .. }
            | AppEvent::StreamPromptProgress { session_id, .. }
            | AppEvent::StreamRoundComplete { session_id, .. }
            | AppEvent::StreamComplete { session_id, .. }
            | AppEvent::StreamError { session_id, .. }
            | AppEvent::ToolCallWarning { session_id, .. }
            | AppEvent::ToolCallStart { session_id, .. }
            | AppEvent::ToolCallProgress { session_id, .. }
            | AppEvent::ToolCallComplete { session_id, .. }
            | AppEvent::ToolCallError { session_id, .. }
            | AppEvent::StreamThinkingChunk { session_id, .. }
            | AppEvent::StreamThinkingComplete { session_id, .. }
            | AppEvent::NCtxUpdated { session_id, .. }
            | AppEvent::ImprovementSuggested { session_id, .. }
            | AppEvent::AgentHandoff { session_id, .. }
            | AppEvent::RestartRequested { session_id, .. }
            | AppEvent::UserMessageDrained { session_id, .. }
            => session_id.clone(),
        };
        match event {
            AppEvent::StreamChunk { content, .. } => {
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.stream_chunk(&content);
                    // Live status-bar estimates while generating (content and
                    // thinking chunks alike): see `update_live_estimates`.
                    runtime.chat_state.update_live_estimates(n_ctx);
                }
            }
            AppEvent::StreamPromptProgress { progress, .. } => {
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    // Live PP progress + speed while the server processes the
                    // prompt (llama.cpp `prompt_progress`; counts only
                    // non-cached tokens). Replaced by the server-reported
                    // final value when the round completes.
                    runtime.chat_state.prompt_progress = Some(progress);
                    if let Some(tps) = progress.prompt_tps() {
                        runtime.chat_state.prompt_tps = Some(tps);
                    }
                }
            }
            AppEvent::StreamRoundComplete { content: _, usage, .. } => {
                // Intermediate tool round: commit the round's text, keep generating
                // so the next round's chunks keep rendering live.
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.commit_stream();
                    // This round's prompt processing is done — drop the live
                    // progress pill (the next round may start a new one).
                    runtime.chat_state.prompt_progress = None;
                    // Gauge: prefer the server's exact usage (input+output tokens of
                    // this round); fall back to the exact char counter of the
                    // stored conversation when the backend omits usage.
                    if let Some(u) = &usage {
                        runtime.chat_state.token_count = u.total_tokens as usize;
                        if n_ctx > 0 {
                            runtime.chat_state.context_used = u.total_tokens as f32 / n_ctx as f32 * 100.0;
                        }
                        // Server speeds (llama.cpp `timings`); None when the
                        // backend doesn't report them — that clears stale
                        // values from a previous, different backend.
                        let t = u.timings.as_ref();
                        runtime.chat_state.prompt_tps = t.and_then(|t| t.prompt_per_second);
                        runtime.chat_state.gen_tps = t.and_then(|t| t.predicted_per_second);
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
                    runtime.chat_state.prompt_progress = None;
                    runtime.chat_state.is_generating = false;
                    runtime.chat_state.current_thinking.clear();
                    // The run ended: any live tool card still open (e.g. a tool
                    // aborted before it emitted its completion) is stale now.
                    runtime.chat_state.active_tools.clear();
                    if is_selected {
                        self.status = AppStatus::Ready;
                    }
                    runtime.chat_state.status = AppStatus::Ready;
                    if let Some(usage) = usage {
                        runtime.chat_state.token_count = usage.total_tokens as usize;
                        if n_ctx > 0 {
                            runtime.chat_state.context_used = usage.total_tokens as f32 / n_ctx as f32 * 100.0;
                        }
                        // Server speeds (llama.cpp `timings`); None when the
                        // backend doesn't report them.
                        let t = usage.timings.as_ref();
                        runtime.chat_state.prompt_tps = t.and_then(|t| t.prompt_per_second);
                        runtime.chat_state.gen_tps = t.and_then(|t| t.predicted_per_second);
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
                    runtime.chat_state.prompt_progress = None;
                    runtime.chat_state.is_generating = false;
                    // Aborted tools never emit their completion — drop their
                    // live cards so the transcript doesn't spin forever.
                    runtime.chat_state.active_tools.clear();
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
            AppEvent::ToolCallStart { tool_name, call_id, args_preview, .. } => {
                tracing::debug!(tool_name, call_id, "Tool call started");
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    // Open a live card for this call.
                    if !runtime
                        .chat_state
                        .active_tools
                        .iter()
                        .any(|t| t.call_id == call_id)
                    {
                        runtime.chat_state.active_tools.push(wuffagent_core::sessions::ActiveTool {
                            tool_name: tool_name.clone(),
                            call_id: call_id.clone(),
                            args_preview,
                            started_at: std::time::Instant::now(),
                            live_output: String::new(),
                        });
                    }
                }
            }
            AppEvent::ToolCallProgress { tool_name, call_id, text, .. } => {
                // Live output tail for the running tool card (latest-tail:
                // replace, don't append).
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    if let Some(active) = runtime
                        .chat_state
                        .active_tools
                        .iter_mut()
                        .find(|t| t.call_id == call_id)
                    {
                        active.live_output = text;
                    }
                    let _ = tool_name;
                }
            }
            AppEvent::ToolCallComplete { tool_name, call_id, result, .. } => {
                tracing::debug!(tool_name, result, "Tool call complete");
                // Close the live card, capturing the args preview and duration
                // so the persisted message can show both.
                let (args_preview, duration_ms) = self
                    .session_store
                    .get(&sid)
                    .and_then(|r| {
                        r.chat_state
                            .active_tools
                            .iter()
                            .find(|t| t.call_id == call_id)
                            .map(|t| {
                                (
                                    t.args_preview.clone(),
                                    t.started_at.elapsed().as_millis() as u64,
                                )
                            })
                    })
                    .unwrap_or_default();
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime
                        .chat_state
                        .active_tools
                        .retain(|t| t.call_id != call_id);
                    let header = if args_preview.is_empty() {
                        wuffagent_core::tools::tool_call_header(&tool_name, &result)
                    } else {
                        format!("🔧 {}: {}", tool_name, args_preview)
                    };
                    let content = if duration_ms > 0 {
                        format!("{}||{}||{}||{}", header, call_id, result, duration_ms)
                    } else {
                        format!("{}||{}||{}", header, call_id, result)
                    };
                    runtime.chat_state.push_message(MessageKind::Tool, "tool", &content);
                }
            }
            AppEvent::ToolCallError { tool_name, call_id, error, .. } => {
                tracing::warn!(tool_name, error, "Tool call error");
                // Close the live card for errors too (duration + args preview).
                let (args_preview, duration_ms) = self
                    .session_store
                    .get(&sid)
                    .and_then(|r| {
                        r.chat_state
                            .active_tools
                            .iter()
                            .find(|t| t.call_id == call_id)
                            .map(|t| {
                                (
                                    t.args_preview.clone(),
                                    t.started_at.elapsed().as_millis() as u64,
                                )
                            })
                    })
                    .unwrap_or_default();
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime
                        .chat_state
                        .active_tools
                        .retain(|t| t.call_id != call_id);
                    let header = if args_preview.is_empty() {
                        format!("Tool '{}' error: {}", tool_name, error)
                    } else {
                        format!("✗ {}: {} — {}", tool_name, args_preview, error)
                    };
                    let content = if duration_ms > 0 {
                        format!("{}||{}||{}||{}", header, call_id, error, duration_ms)
                    } else {
                        format!("{}||{}||{}||", header, call_id, error)
                    };
                    runtime.chat_state.push_message(MessageKind::Tool, "tool", &content);
                }
            }
            AppEvent::StreamThinkingChunk { content, .. } => {
                tracing::trace!("UI: StreamThinkingChunk received, content_len={}", content.len());
                // Live display + status-bar estimates: thinking tokens are
                // generated tokens, so TG speed and the token gauge must
                // track them too (before this, TG froze for the whole
                // duration of thinking segments).
                if let Some(runtime) = self.session_store.get_mut(&sid) {
                    runtime.chat_state.stream_thinking_chunk(&content);
                    runtime.chat_state.update_live_estimates(n_ctx);
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
            AppEvent::UserMessageDrained { message, .. } => {
                // A message the user sent while this run was active arrived
                // too late to be injected into the running agent loop (the
                // run had already ended - e.g. it landed during the final
                // verification call - or the run was cancelled). It was
                // already displayed in the chat at send time, so
                // `already_displayed = true`.
                tracing::info!(text = %message.text, "User message drained after run ended - starting next turn");
                let generating = self
                    .session_store
                    .get(&sid)
                    .map(|r| r.chat_state.is_generating)
                    .unwrap_or(false);
                if generating {
                    // A new run is already in flight (rare race): fall back
                    // to the queue; it is drained when that run ends.
                    if let Some(rt) = self.session_store.get_mut(&sid) {
                        rt.chat_state.queued_messages.push(*message);
                    }
                } else {
                    self.start_pipeline_for_session(
                        &sid,
                        &message.text,
                        message.image,
                        message.agent_prompt,
                        message.tool_policy,
                        true,
                    );
                }
            }
        }
    }
}
