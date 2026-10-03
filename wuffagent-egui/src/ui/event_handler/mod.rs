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
            // 2d: manual "Run evals" finished — not session-bound (the agent
            // editor's eval panel is global).
            AppEvent::EvalsRunFinished { .. } => String::new(),
            // Status bar: LLM-activity snapshot — not session-bound (it is a
            // global view of everything running).
            AppEvent::LlmActivity { .. } => String::new(),
            // Server status monitor — not session-bound (global server state).
            AppEvent::ServerStatus { .. } => String::new(),
        };

        // Host bridge (P1): forward pipeline events to the registered plugin
        // callback (fast, non-blocking — a plugin forwards them into its own
        // mpsc). Kinds: 0 = chunk, 1 = complete, 2 = error, 3 = round-complete.
        match &event {
            AppEvent::StreamChunk { content, .. } => {
                crate::host_bridge::emit_event(0, &sid, content)
            }
            AppEvent::StreamComplete { content, .. } => {
                crate::host_bridge::emit_event(1, &sid, content)
            }
            AppEvent::StreamError { error, .. } => crate::host_bridge::emit_event(2, &sid, error),
            AppEvent::StreamRoundComplete { .. } => crate::host_bridge::emit_event(3, &sid, ""),
            _ => {}
        }
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
                // Fork the clean sub-session: create its session file and a
                // FRESH runtime (a new ChatClient whose session meta carries
                // `parent_session_id` back to the forking session — so its
                // first save persists the parent link and the core
                // advertises `hand_back` to it), open its tab, and start its
                // first turn with the handoff task. The parent is already
                // idle + saved: the core sent its terminal StreamComplete
                // before this event.
                tracing::info!(
                    %parent_session_id,
                    %agent,
                    "Sub-session handoff: forking sub-session"
                );
                let sessions_dir = self.core.config.sessions_dir.clone();
                let sub_name = format!("Sub: {}", agent);
                let session = wuffagent_core::sessions::create_session(&sessions_dir, &sub_name);
                let event_tx = self
                    .relay
                    .pending_tx
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .clone();
                let mut runtime = wuffagent_core::sessions::SessionRuntime::create_from_config(
                    &self.core.config,
                    &self.core.connection,
                    &self.core.agent_engine,
                    session.id.clone(),
                    session.name.clone(),
                    event_tx,
                );
                runtime.selected_agent = Some(agent.clone());
                runtime
                    .client
                    .set_session_meta(wuffagent_core::sessions::SessionMeta {
                        selected_agent: runtime.selected_agent.clone(),
                        reasoning_mode: runtime.reasoning_mode,
                        parent_session_id: Some(parent_session_id),
                    });
                self.sessions
                    .session_store
                    .insert(session.id.clone(), runtime);
                // Open the sub-session tab and make it active; the main tab
                // (the parent, still the selected session) stays available.
                self.sessions.sub_session_tabs.push(session.id.clone());
                self.sessions.active_tab = Some(session.id.clone());
                if let Some(panel) = self.sessions_panel.as_mut() {
                    panel.refresh();
                    panel.show_notification(
                        &format!(
                            "Sub-session '{}' started for agent '{}'",
                            session.name, agent
                        ),
                        true,
                    );
                }
                // Start the first turn in the sub-session: fresh context — the
                // handoff task is the only message it starts with.
                self.start_pipeline_for_session(
                    &session.id,
                    &task,
                    None,
                    self.resolve_agent_prompt(&agent),
                    self.resolve_tool_policy(&agent),
                    false,
                );
            }
            AppEvent::AgentHandBack {
                from_session_id,
                to_session_id,
                task,
            } => {
                // The sub-session finished and handed the session back to its
                // parent: focus the parent (main tab + sidebar selection) and
                // post the hand-back task as the parent's next turn — queued
                // (displayed immediately, run when the current turn ends) if
                // the parent is still generating.
                tracing::info!(
                    %from_session_id,
                    %to_session_id,
                    "Agent hand-back: returning session to parent"
                );
                self.sessions.active_tab = None;
                self.sessions.selected_session_id = Some(to_session_id.clone());
                if let Some(panel) = self.sessions_panel.as_mut() {
                    *panel.selected_id_mut() = Some(to_session_id.clone());
                    panel.refresh();
                }
                // Resolve prompt/policy from the PARENT's selected agent (the
                // same resolution the input box uses at send time).
                let agent = self
                    .sessions
                    .session_store
                    .get(&to_session_id)
                    .and_then(|r| r.selected_agent.clone())
                    .unwrap_or_default();
                let (agent_prompt, tool_policy) = (
                    self.resolve_agent_prompt(&agent),
                    self.resolve_tool_policy(&agent),
                );
                let generating = self
                    .sessions
                    .session_store
                    .get(&to_session_id)
                    .map(|r| r.chat_state.is_generating)
                    .unwrap_or(false);
                if generating {
                    if let Some(rt) = self.sessions.session_store.get_mut(&to_session_id) {
                        // Displayed at queue time; the drain starts it with
                        // `already_displayed = true`.
                        rt.chat_state.push_message(
                            MessageKind::Normal,
                            "user",
                            &format!("🔁 Hand-back: {}", task),
                        );
                        rt.chat_state
                            .queued_messages
                            .push(wuffagent_core::types::QueuedMessage {
                                text: task,
                                image: None,
                                agent_prompt,
                                tool_policy,
                            });
                    }
                } else {
                    self.start_pipeline_for_session(
                        &to_session_id,
                        &task,
                        None,
                        agent_prompt,
                        tool_policy,
                        false,
                    );
                }
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
            // 2d: a manual "Run evals" finished — clear the running flag and
            // show the pass/fail summary in the agent editor's eval panel.
            AppEvent::EvalsRunFinished {
                agent_name,
                summary,
            } => {
                tracing::info!(%agent_name, "Manual eval run finished");
                if let Some(d) = self.dialogs.agent_config_dialog.as_mut() {
                    d.mark_evals_finished(&agent_name, &summary);
                }
            }
            // Status bar: whole-snapshot replacement of the live LLM activities
            // (the tracker emits the full list on every event; an empty list
            // means nothing is running anymore).
            AppEvent::LlmActivity { activities } => {
                self.display.llm_activities = activities;
            }
            // Server status monitor: update the shared server status snapshot
            // (the UI reads this each frame for the server status indicator).
            AppEvent::ServerStatus { status } => {
                if let Ok(mut guard) = self.core.server_status.lock() {
                    *guard = status;
                }
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
