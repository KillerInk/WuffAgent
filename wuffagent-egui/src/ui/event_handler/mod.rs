use super::state::ChatApp;
use wuffagent_core::types::{AppEvent, MessageKind};

mod stream;
mod tool;

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
            | AppEvent::McpConfigChanged { session_id, .. }
            | AppEvent::RestartRequested { session_id, .. }
            | AppEvent::UserMessageDrained { session_id, .. } => session_id.clone(),
            // The sub-session events carry no `session_id`: route them by
            // the explicit ids they own. `SubSessionHandoff` concerns the
            // parent (whose turn just ended); `AgentHandBack` the sub-session
            // (whose turn just ended).
            AppEvent::SubSessionHandoff {
                parent_session_id, ..
            } => parent_session_id.clone(),
            AppEvent::AgentHandBack {
                from_session_id, ..
            } => from_session_id.clone(),
            // 4b: manual "run check now" finished — not session-bound (the
            // improvements panel is global).
            AppEvent::ImprovementCheckFinished { .. } => String::new(),
        };
        match event {
            // Stream lifecycle arms: see `stream.rs`.
            AppEvent::StreamChunk { content, .. } => {
                self.handle_stream_chunk(&content, &sid, n_ctx)
            }
            AppEvent::StreamPromptProgress { progress, .. } => {
                self.handle_stream_prompt_progress(progress, &sid)
            }
            AppEvent::StreamRoundComplete { usage, .. } => {
                self.handle_stream_round_complete(usage.as_ref(), &sid, n_ctx)
            }
            AppEvent::StreamComplete { content, usage, .. } => {
                self.handle_stream_complete(&content, usage, &sid, n_ctx)
            }
            AppEvent::StreamError { error, .. } => self.handle_stream_error(&error, &sid),
            AppEvent::StreamThinkingChunk { content, .. } => {
                self.handle_thinking_chunk(&content, &sid, n_ctx)
            }
            AppEvent::StreamThinkingComplete { .. } => self.handle_thinking_complete(&sid),
            // Tool-call arms: see `tool.rs`.
            AppEvent::ToolCallWarning {
                tool_name, message, ..
            } => self.handle_tool_call_warning(&tool_name, &message),
            AppEvent::ToolCallStart {
                tool_name,
                call_id,
                args_preview,
                ..
            } => self.handle_tool_call_start(&tool_name, &call_id, args_preview, &sid),
            AppEvent::ToolCallProgress {
                tool_name,
                call_id,
                text,
                ..
            } => self.handle_tool_call_progress(&tool_name, &call_id, text, &sid),
            AppEvent::ToolCallComplete {
                tool_name,
                call_id,
                result,
                ..
            } => self.handle_tool_call_complete(&tool_name, &call_id, result, &sid),
            AppEvent::ToolCallError {
                tool_name,
                call_id,
                error,
                ..
            } => self.handle_tool_call_error(&tool_name, &call_id, error, &sid),
            AppEvent::NCtxUpdated {
                n_ctx,
                session_id: _,
            } => {
                tracing::info!(n_ctx, "n_ctx updated");
            }
            AppEvent::AgentHandoff { from, to, task, .. } => {
                tracing::info!(from, to, "Agent handoff");
                if let Some(runtime) = self.sessions.session_store.get_mut(&sid) {
                    // The session is now owned by the target agent: follow the
                    // switch so follow-up messages (and the next queued one)
                    // run under the new profile.
                    runtime.selected_agent = Some(to.clone());
                    // Persist the switch with the session file on the next
                    // save. Update only the selection; cloning the existing
                    // meta first keeps non-UI fields (the sub-session parent
                    // link) intact.
                    let mut meta = runtime.client.session_meta().clone();
                    meta.selected_agent = runtime.selected_agent.clone();
                    meta.reasoning_mode = runtime.reasoning_mode;
                    runtime.client.set_session_meta(meta);
                    // Visible banner in the chat transcript.
                    runtime.chat_state.push_message(
                        MessageKind::Normal,
                        "system",
                        &format!("🔀 Handoff: {} → {} — {}", from, to, task),
                    );
                }
            }
            AppEvent::SubSessionHandoff {
                parent_session_id,
                agent,
                task,
            } => {
                // Placeholder (step 16 wires the full handler: create the sub
                // runtime, open its tab, post the first turn).
                tracing::info!(
                    parent_session_id,
                    agent,
                    "Sub-session handoff (handler pending, step 16)"
                );
                let _ = task;
            }
            AppEvent::AgentHandBack {
                from_session_id,
                to_session_id,
                task,
            } => {
                // Placeholder (step 17 wires the full handler: focus the
                // parent tab, post the hand-back turn into the parent).
                tracing::info!(
                    from_session_id,
                    to_session_id,
                    "Agent hand-back (handler pending, step 17)"
                );
                let _ = task;
            }
            AppEvent::ImprovementSuggested {
                agent_name,
                suggestions,
                session_id: _,
            } => {
                tracing::info!(
                    agent_name,
                    count = suggestions.len(),
                    "Improvement suggestions received"
                );
                self.dialogs
                    .improvements_panel
                    .handle_improvement_suggested(&agent_name, suggestions);
            }
            // 4b: a manual "run check now" finished — clear the running flag
            // and record the outcome (suggestions, when produced, already
            // arrived via ImprovementSuggested above).
            AppEvent::ImprovementCheckFinished {
                agent_name,
                produced,
            } => {
                tracing::info!(%agent_name, produced, "On-demand improvement check finished");
                self.dialogs
                    .improvements_panel
                    .mark_check_finished(&agent_name, produced);
            }
            AppEvent::McpConfigChanged { .. } => {
                // An MCP management tool rewrote config.json's mcp_servers
                // array; the in-memory list is stale. Reload it so the MCP
                // panel (and any later reload) matches disk.
                match wuffagent_core::config::Config::load(&self.core.config.file_path.clone()) {
                    Ok(loaded) => {
                        tracing::info!(
                            count = loaded.mcp_servers.len(),
                            "MCP config reloaded after McpConfigChanged"
                        );
                        self.core.config.mcp_servers = loaded.mcp_servers;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to reload MCP config after McpConfigChanged: {}", e);
                    }
                }
            }
            AppEvent::RestartRequested {
                reason, exe_path, ..
            } => {
                tracing::info!(reason, "Restart requested");
                // Persist the transcript so nothing is lost across the relaunch,
                // show a banner, then relaunch the (optionally newly built)
                // binary — the marker + auto-resume pick the work back up.
                if let Err(e) = self.save_session_for(&sid) {
                    tracing::warn!("Failed to save session before restart: {}", e);
                }
                if let Some(runtime) = self.sessions.session_store.get_mut(&sid) {
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
                    .sessions
                    .session_store
                    .get(&sid)
                    .map(|r| r.chat_state.is_generating)
                    .unwrap_or(false);
                if generating {
                    // A new run is already in flight (rare race): fall back
                    // to the queue; it is drained when that run ends.
                    if let Some(rt) = self.sessions.session_store.get_mut(&sid) {
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
