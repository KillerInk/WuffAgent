//! `Agent::execute` — entry point + handoff/restart routing (extracted A5).
//!
//! Runs the LLM loop for one turn, follows in-turn `handoff` tool calls
//! across agent profiles on the same conversation store (capped by
//! `MAX_HANDOFF_DEPTH`), resolves `restart` requests into UI events, and
//! forks `sub_session` handoffs: the parent turn ends (marker + terminal
//! `StreamComplete` + `SubSessionHandoff`) and the UI creates the clean
//! sub-session. `hand_back` requests end the sub-session turn the same way
//! (`AgentHandBack`, step 11).

use tokio_util::sync::CancellationToken;

use crate::types::Message;

use super::r#loop::RunOutcome;
use super::Agent;

/// Maximum number of handoff hops within a single user turn. Bounds
/// handoff loops (A→B→A→…) — each hop is a full agent run, so this also
/// caps the total work a single queued turn can trigger.
pub(crate) const MAX_HANDOFF_DEPTH: usize = 8;

impl Agent {
    /// Execute a request with this agent.
    ///
    /// `image` is an optional `data:` URI (e.g. `data:image/png;base64,...`)
    /// for an image attached by the user. It is stored on the user message in
    /// the conversation store, which makes it part of every subsequent LLM
    /// request (serialized as an OpenAI-style `image_url` content part) and
    /// persisted with the session.
    pub async fn execute(
        &mut self,
        request: &str,
        image: Option<&str>,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }

        // Turn start: record the user message in the shared store exactly once.
        // The store (`client.conversation`) is the single source of truth for
        // history; the system prompt is kept out of it and rebuilt per request,
        // exactly like the client's non-agent streaming path.
        {
            let mut conv = self.client.conversation().lock().unwrap();
            conv.push(Message {
                role: "user".to_string(),
                content: request.to_string(),
                timestamp: crate::types::format_timestamp(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                image: image.map(str::to_string),
            });
        }

        // Throwaway request list: fresh system prompt + a snapshot of the store
        // (which already contains the user message from the step above).
        let mut messages = self.build_initial_messages(request);

        // ── Handoff chain ────────────────────────────────────────────────
        // If the running agent calls the `handoff` tool, its loop returns
        // RunOutcome::Handoff; we then switch to a fresh Agent for the
        // target profile, which continues on the SAME conversation store
        // (shared client) with its own system prompt, tools, shell, and
        // reasoning effort. Hops are capped at MAX_HANDOFF_DEPTH to break
        // handoff loops (A→B→A→…).
        let mut outcome = self.run_llm_loop(&mut messages, cancel_token).await?;
        let mut hops: usize = 0;
        // The name of the agent currently running the loop (the original
        // agent for hop 0; the previous hop's target afterwards) so multi-hop
        // chains report "B -> C", not "A -> C".
        let mut current_name = self.config.name.clone();
        loop {
            // Match on a reference: the sub-session handoff must break
            // WITHOUT moving `outcome` so the final match below can consume
            // it; the in-turn case clones the request (cheap — a few strings
            // and a profile).
            let mut req = match &outcome {
                RunOutcome::Completed(_) => break,
                // A restart request ends the run (handled in the final match
                // below); `_` keeps `outcome` un-moved like the Completed arm.
                RunOutcome::Restart(_) => break,
                // A hand-back request also ends the run (handled in the
                // final match below) — it is not part of the in-turn chain.
                RunOutcome::HandBack(_) => break,
                // A sub-session handoff ends the run too: the fork (marker,
                // terminal StreamComplete, SubSessionHandoff) is handled in
                // the final match below.
                RunOutcome::Handoff(r) if r.sub_session => break,
                RunOutcome::Handoff(r) => r.clone(),
            };
            hops += 1;
            if hops > MAX_HANDOFF_DEPTH {
                return Err(format!(
                    "Handoff chain exceeded {MAX_HANDOFF_DEPTH} hops; stopping (possible handoff loop)"
                ));
            }
            if cancel_token.is_cancelled() {
                return Err("Cancelled".to_string());
            }

            let from = current_name.clone();
            tracing::info!(
                "[AGENT] Handing off the session: {} -> {} (task: {})",
                from,
                req.agent,
                req.task
            );

            // Record the handoff in the store as a user-role marker so the
            // target agent's snapshot (and every later turn, including after
            // a session reload) sees the transition and why it happened.
            let marker = Message {
                role: "user".to_string(),
                content: format!("[Handoff from '{}' to '{}'] {}", from, req.agent, req.task),
                timestamp: crate::types::format_timestamp(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                image: None,
            };
            self.record_in_store(&marker);
            self.send_event(crate::types::AppEvent::AgentHandoff {
                from: from.clone(),
                to: req.agent.clone(),
                task: req.task.clone(),
                session_id: self.session_id(),
            });

            // The target agent gets a FRESH Agent (own system prompt, tool
            // schema, shell/handoff swaps, client clone with its reasoning
            // effort, trimming state) but shares the conversation store,
            // event channel, memory manager, and session id. `self.tool_manager`
            // (already the per-execution manager) is the shared base; the
            // target's builder applies its own shell/handoff swaps on top.
            // The task timeout is a property of the SESSION run, not the
            // profile: the target inherits the original agent's value so a
            // no-timeout chat run (task_timeout_ms=0) stays timeout-free
            // across every hop. Otherwise a profile default (e.g. 60s for
            // coder) would force-kill long tasks after a handoff.
            req.config.task_timeout_ms = self.config.task_timeout_ms;
            let mut next = Self::builder(
                req.config.clone(),
                self.llm_client.clone(),
                self.client.clone(),
            )
            .tool_manager(self.tool_manager.clone())
            .event_tx(self.event_tx.clone())
            .memory(self.memory.clone())
            .agent_session_id(self.agent_session_id.clone())
            .build();
            // The whole handoff chain is still the same turn: keep receiving
            // user injections on the next agent too.
            next.injection_rx = self.injection_rx.take();
            // Memory injection is query-aware on the handoff task.
            let mut next_messages = next.build_initial_messages(&req.task);
            current_name = req.agent.clone();
            outcome = next.run_llm_loop(&mut next_messages, cancel_token).await?;
        }

        // Keep a clean copy for memory extraction: only the current turn's
        // window (from this turn's user message on), not the whole store —
        // the store grows with the session, so a full clone here would cost
        // more every turn. Assistant/tool messages were already recorded to
        // the store as they were generated inside run_llm_loop (across every
        // hop), so there is no sync-back. The store may have been
        // reconciled/trimmed mid-turn, so locate the turn's user message by
        // its last occurrence rather than by a captured index.
        {
            let conv = self.client.conversation().lock().unwrap();
            let turn_idx = conv
                .iter()
                .rposition(|m| m.role == "user" && m.content == request);
            self.messages = match turn_idx {
                Some(i) => conv.iter().skip(i).cloned().collect(),
                // The turn's user message was trimmed away (extreme context
                // pressure) — fall back to whatever the store still holds.
                None => conv.clone(),
            };
        }

        // Persistence is owned by the UI (it saves on StreamComplete, for both the
        // agent and non-agent paths), so the agent does not write the session file
        // itself. This keeps a single writer per store and avoids a redundant save.

        match outcome {
            RunOutcome::Completed(content) => Ok(content),
            RunOutcome::Handoff(req) => {
                if !req.sub_session {
                    unreachable!("in-turn handoff outcomes are consumed by the chain loop")
                }
                // Sub-session fork: end the parent turn without chaining.
                // Record the fork in the parent store (a user-role marker,
                // like the in-turn handoff marker) so the transcript — and
                // every later turn, including after a reload — shows where
                // the work went.
                let marker_content = format!(
                    "[Handed off to '{}' (sub-session)] {}",
                    req.agent, req.task
                );
                let marker = Message {
                    role: "user".to_string(),
                    content: marker_content.clone(),
                    timestamp: crate::types::format_timestamp(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    image: None,
                };
                self.record_in_store(&marker);
                // Terminal StreamComplete for the parent: clears its
                // generating state and persists the parent session (the
                // marker is already in the store). If the parent streamed
                // text this turn the UI commits that buffer; the marker
                // text is only the fallback for a completely silent turn.
                self.send_event(crate::types::AppEvent::StreamComplete {
                    content: marker_content,
                    usage: None,
                    session_id: self.session_id(),
                });
                // Tell the UI to fork the clean sub-session and start its
                // first turn with `task`. Sent AFTER the StreamComplete so
                // the parent is idle and saved before the sub-session runs.
                self.send_event(crate::types::AppEvent::SubSessionHandoff {
                    parent_session_id: self.session_id(),
                    agent: req.agent,
                    task: req.task,
                });
                Ok("Handed off to a sub-session".to_string())
            }
            // Hand-back: this session is a sub-session returning to its
            // parent. Record a marker in the SUB store so the transcript
            // shows the return, emit AgentHandBack (both session ids) so the
            // UI can post the task into the parent session, and end the turn
            // with a terminal StreamComplete for this (sub) session so its
            // generating state clears and its file saves.
            RunOutcome::HandBack(req) => {
                let to_session_id = self
                    .client
                    .session_meta()
                    .parent_session_id
                    .clone()
                    .expect("hand_back is only advertised to sub-sessions (parent_session_id set)");
                let marker_content = format!("[Handed back to parent session] {}", req.task);
                let marker = Message {
                    role: "user".to_string(),
                    content: marker_content.clone(),
                    timestamp: crate::types::format_timestamp(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    image: None,
                };
                self.record_in_store(&marker);
                // Terminal StreamComplete for the sub session: if the agent
                // streamed text this turn the UI commits that buffer; the
                // marker text is only the fallback for a silent turn.
                self.send_event(crate::types::AppEvent::StreamComplete {
                    content: marker_content,
                    usage: None,
                    session_id: self.session_id(),
                });
                self.send_event(crate::types::AppEvent::AgentHandBack {
                    from_session_id: self.session_id(),
                    to_session_id,
                    task: req.task,
                });
                Ok("Handed back to the parent session".to_string())
            }
            // A restart request ends the turn: record a marker so the session
            // shows the transition, then notify the UI to relaunch the
            // (optionally newly built) binary. The marker file + auto-resume
            // pick the work back up after the process restarts, so report
            // success — the return value is not meaningful here.
            RunOutcome::Restart(req) => {
                let reason = req.reason.clone();
                let marker = Message {
                    role: "user".to_string(),
                    content: format!("[Restart requested] {}", reason),
                    timestamp: crate::types::format_timestamp(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    image: None,
                };
                self.record_in_store(&marker);
                self.send_event(crate::types::AppEvent::RestartRequested {
                    reason,
                    build_cmd: req.build_cmd,
                    exe_path: req.exe_path,
                    session_id: self.session_id(),
                });
                Ok("Restart requested".to_string())
            }
        }
    }
}
