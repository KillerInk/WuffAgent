//! Agent execution: `execute` (handoff/restart routing) and `run_llm_loop`
//! (the streaming tool-call loop). Split out of agents/agent.rs (A1).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use tokio_util::sync::CancellationToken;

use super::truncate_chars;
use super::Agent;
use super::VERIFICATION_NUDGE;
use crate::client::ChatClient;
use crate::types::Message;

/// Minimum delay between LLM calls to prevent API rate-limiting (500ms).
const LLM_RATE_LIMIT_DELAY: Duration = Duration::from_millis(500);
/// Maximum verification attempts before giving up.
const MAX_VERIFICATION_ATTEMPTS: u32 = 2;
/// Maximum number of handoff hops within a single user turn. Bounds
/// handoff loops (A→B→A→…) — each hop is a full agent run, so this also
/// caps the total work a single queued turn can trigger.
const MAX_HANDOFF_DEPTH: usize = 8;

/// Rough char allowance per attached image: the server turns the image into
/// a model-dependent number of vision tokens (the base64 payload size is
/// unrelated to that count), so a fixed ~2k-token (~8k-char at 4 chars/token)
/// allowance is reserved per image instead of counting the payload.
pub(crate) const ESTIMATED_IMAGE_CHARS: usize = 8_000;

/// Per-request char overhead that `message_char_count` never sees: the
/// serialized tool schemas (sent with EVERY request) plus a fixed allowance
/// per attached image. The trim budget is a percentage of n_ctx in char
/// units, so it must reserve this — otherwise the request can exceed n_ctx
/// while the message list alone is still below the 90% trigger (and even
/// after trimming to the 50% target or the 85% overflow-retry budget).
pub(crate) fn request_overhead_chars(
    tool_defs: Option<&[crate::tools::ToolDefinition]>,
    messages: &[Message],
) -> usize {
    let schema_chars = tool_defs
        .map(|defs| serde_json::to_string(defs).map(|s| s.chars().count()).unwrap_or(0))
        .unwrap_or(0);
    let images = messages.iter().filter(|m| m.image.is_some()).count();
    schema_chars + images * ESTIMATED_IMAGE_CHARS
}
/// Outcome of one agent's LLM loop.
pub(crate) enum RunOutcome {
    /// The turn completed; the assistant's final text.
    Completed(String),
    /// The `handoff` tool was called; `execute` switches to the target agent
    /// on the same conversation store.
    Handoff(crate::agents::types::HandoffRequest),
    /// The `restart` tool was called; `execute` emits `RestartRequested` so
    /// the UI can relaunch the (optionally newly built) binary and resume
    /// this session automatically.
    Restart(crate::agents::types::RestartRequest),
}

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
            let mut req = match outcome {
                RunOutcome::Completed(_) => break,
                // A restart request ends the run (handled in the final match
                // below); `_` keeps `outcome` un-moved like the Completed arm.
                RunOutcome::Restart(_) => break,
                RunOutcome::Handoff(req) => req,
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
            // target's Agent::new applies its own shell/handoff swaps on top.
            // The task timeout is a property of the SESSION run, not the
            // profile: the target inherits the original agent's value so a
            // no-timeout chat run (task_timeout_ms=0) stays timeout-free
            // across every hop. Otherwise a profile default (e.g. 60s for
            // coder) would force-kill long tasks after a handoff.
            req.config.task_timeout_ms = self.config.task_timeout_ms;
            let mut next = Self::new(
                req.config.clone(),
                self.llm_client.clone(),
                self.tool_manager.clone(),
                self.event_tx.clone(),
                self.client.clone(),
                self.memory.clone(),
                self.agent_session_id.clone(),
            );
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
            RunOutcome::Handoff(_) => {
                unreachable!("handoff outcomes are consumed by the chain loop")
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

    /// Convert an attached image (egui source from the UI) into the `data:` URI
    /// form used in model requests. Only `Bytes` sources carry a payload to send;
    /// texture/URI references have none and return `None`.
    pub(crate) fn image_source_data_uri(source: &egui::ImageSource<'static>) -> Option<String> {
        let bytes = match source {
            egui::ImageSource::Bytes { bytes, .. } => bytes,
            _ => return None,
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes.as_ref());
        Some(format!("data:image/png;base64,{}", b64))
    }

    /// Run the LLM loop with NATIVE tool calling via the chat client (SSE).
    ///
    /// - Streams content/thinking to the UI through the agent event channel.
    /// - Sends the agent's tool definitions (filtered by `allowed_tools`) so
    ///   the model can emit structured tool calls, executed in-process.
    /// - Round-trips the model's `reasoning_content` in history so reasoning
    ///   models (Qwen3/DeepSeek style) stay coherent across tool-call rounds.
    /// - Falls back to text-embedded tool-call parsing (bash blocks / JSON
    ///   arrays) for models that don't honor native function calling.
    pub(crate) async fn run_llm_loop(
        &mut self,
        messages: &mut Vec<Message>,
        cancel_token: &CancellationToken,
    ) -> Result<RunOutcome, String> {
        let mut verification_attempts = 0u32;
        // S1: the last NEEDS_FIX reason seen this run, for the outcome memory.
        let mut last_failed_judge_reason = String::new();
        let start = Instant::now();

        // Per-agent tool manager, filtered by `allowed_tools`. An empty list
        // means "all tools". Used for BOTH the tool definitions sent to the
        // model and the execution of its tool calls, so the model can only
        // ever see and run tools it is authorized for.
        let tool_manager: crate::tools::ToolManager = {
            let manager = self.tool_manager.lock().unwrap();
            if self.config.allowed_tools.is_empty() {
                manager.clone()
            } else {
                // `shell` and `handoff` are gated by their `*_enabled` flags,
                // not by `allowed_tools`, so enabled ones must survive the
                // allowlist filter.
                let mut allowlist = self.config.allowed_tools.clone();
                if self.config.get_shell_config().shell_enabled
                    && !allowlist.iter().any(|t| t == "shell")
                {
                    allowlist.push("shell".to_string());
                }
                if self.config.handoff_enabled && !allowlist.iter().any(|t| t == "handoff") {
                    allowlist.push("handoff".to_string());
                }
                if self.config.restart_enabled && !allowlist.iter().any(|t| t == "restart") {
                    allowlist.push("restart".to_string());
                }
                manager.with_allowlist(&allowlist)
            }
        };

        // Tool definitions for native function calling, filtered per-agent.
        let tool_defs: Option<Vec<crate::tools::ToolDefinition>> = {
            let defs = tool_manager.get_tool_definitions();
            if defs.is_empty() {
                None
            } else {
                Some(defs)
            }
        };

        // Capture the turn's original request ONCE, before any verification
        // nudge is pushed: after a NEEDS_FIX the last user message in
        // `messages` is the nudge, so re-extracting per verification attempt
        // would make the judge grade the response against the nudge text
        // instead of what the user actually asked.
        //
        // `mut`: mid-run user injections (the chat pipeline's injection
        // channel) extend the turn's request as they arrive, so the
        // verification judge grades the final response against the FULL
        // request — original text plus everything the user added.
        let mut original_request = self.extract_original_request(messages);

        // I1: mark where THIS run's messages start, so trajectory stats can
        // be counted without including earlier turns of the conversation.
        let run_start_len = messages.len();
        let outcome: RunOutcome = loop {
            if cancel_token.is_cancelled() {
                return Err("Cancelled".to_string());
            }

            // ── Mid-run user injections ───────────────────────────────────
            // Messages the user sent while this run is active arrive on the
            // injection channel (the UI pushes them in at send time instead
            // of queueing them behind the whole run). The model can only see
            // new input at an LLM round boundary, so the top of the loop —
            // just before the next LLM call — is the earliest point one can
            // land: append each message to the current turn (request list +
            // shared store) and let the next round react to it. Drained
            // BEFORE the handoff/restart checks so a message sent during a
            // long tool call (e.g. a `restart` build) is recorded in the
            // store and survives the handoff snapshot / process relaunch.
            if let Some(holder) = &self.injection_rx {
                let rx = holder.lock().unwrap();
                while let Ok(injected) = rx.try_recv() {
                    let image = injected
                        .image
                        .as_ref()
                        .and_then(Self::image_source_data_uri);
                    let user_msg = Message {
                        role: "user".to_string(),
                        content: injected.text.clone(),
                        timestamp: crate::types::format_timestamp(),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                        image,
                    };
                    tracing::info!(
                        "[AGENT] Agent '{}' injecting user message sent mid-run into the running turn: {}",
                        self.config.name,
                        injected.text
                    );
                    messages.push(user_msg.clone());
                    self.record_in_store(&user_msg);
                    // The injected message is part of this turn's request now.
                    original_request.push_str(&format!(
                        "\n[User added while the agent was working: {}]",
                        injected.text
                    ));
                }
            }

            // A pending handoff (written by the `handoff` tool this turn)
            // ends this agent's run: `execute` switches to the target agent.
            if let Some(req) = self.take_pending_handoff() {
                tracing::info!(
                    "[AGENT] Agent '{}' handoff requested via the handoff tool; ending this agent's turn (to='{}')",
                    self.config.name,
                    req.agent
                );
                return Ok(RunOutcome::Handoff(req));
            }

            // A pending restart (written by the `restart` tool this turn, its
            // build already finished) ends this agent's run: `execute` emits
            // RestartRequested so the UI can relaunch the (optionally newly
            // built) binary and resume the session automatically.
            if let Some(req) = self.take_pending_restart() {
                tracing::info!(
                    "[AGENT] Agent '{}' restart requested via the restart tool; ending this agent's turn (reason='{}')",
                    self.config.name,
                    req.reason
                );
                return Ok(RunOutcome::Restart(req));
            }

            // Rate-limit LLM calls to avoid hitting API rate limits.
            let elapsed = self.last_llm_call_at.elapsed();
            if elapsed < LLM_RATE_LIMIT_DELAY {
                tokio::time::sleep(LLM_RATE_LIMIT_DELAY - elapsed).await;
            }

            // Check timeout
            if self.config.task_timeout_ms > 0
                && start.elapsed().as_millis() > self.config.task_timeout_ms as u128
            {
                return Err(format!(
                    "Agent '{}' timed out after {}ms",
                    self.config.name, self.config.task_timeout_ms
                ));
            }

            // ── Token-budget trim before each LLM call ──────────────────
            // The agent keeps its own message history (not the client's),
            // so we must trim it manually. Without this, a single large
            // tool result (100k+ tokens) can exceed n_ctx and the server
            // rejects the request.
            //
            // Two thresholds: history grows freely until it crosses the
            // trigger (90% of n_ctx); once it does, we do NOT stop just
            // under the limit — we trim all the way down to the target
            // (50% of n_ctx) so the following rounds have headroom.
            if self.client.n_ctx() > 0 {
                let msg_count = messages.len();
                // The request also carries the tool schemas and attached
                // images, which `message_char_count` does not count — reserve
                // that overhead so the budget covers the ACTUAL request size.
                let overhead_chars = request_overhead_chars(tool_defs.as_deref(), messages);
                let total_chars = crate::trimming::message_char_count(messages) + overhead_chars;
                if msg_count > 4 && total_chars > self.client.trim_trigger_chars() {
                    // Target in char units: 50% of n_ctx tokens converted to
                    // chars via the client's calibrated chars-per-token ratio,
                    // minus the per-request overhead so messages + overhead
                    // together stay under the target.
                    let target_chars = self.client.trim_target_chars().saturating_sub(overhead_chars);
                    let removed = self.trimming.trim_messages(
                        messages,
                        target_chars,
                        &self.config.trim_config,
                    );
                    if removed > 0 {
                        tracing::info!(
                            "[AGENT] Agent '{}' trimmed {} messages (n_ctx={}, target_chars={})",
                            self.config.name,
                            removed,
                            self.client.n_ctx(),
                            target_chars
                        );
                    }
                    // Post-trim verification: the trim already truncates the largest
                    // message as a fallback; log if we're still over budget.
                    let post_trim_total = crate::trimming::message_char_count(messages);
                    if post_trim_total > target_chars {
                        tracing::warn!(
                            "[AGENT] Post-trim count {} > target {}",
                            post_trim_total,
                            target_chars
                        );
                    }
                    // Reconcile the shared store with the trimmed request list.
                    // On the agent path nothing else trims the store, so this
                    // is what keeps it (and the session file) bounded. Runs
                    // whenever the trim pass ran — in-place summarization
                    // shrinks message content even when `removed` is 0, and
                    // the store must mirror that too.
                    self.reconcile_store(messages);
                }
            }

            // ── LLM call (streaming with native tools) ──────────────────
            // The callback must be 'static, so it captures cloned Arcs rather
            // than `self`.
            let round_thinking = Arc::new(Mutex::new(String::new()));

            // ── Parallel tool execution ─────────────────────────────────
            // While the model is still streaming (often: still reasoning), a
            // tool call becomes executable the moment the stream moves past
            // it. The ready callback below spawns its execution in the
            // background at that point, so tools run while the model keeps
            // thinking. Results are collected in call order after the stream
            // ends (see the native tool-call block further down).
            let pending_tool_runs: Arc<super::tool_exec::PendingToolRuns> = Arc::new(
                super::tool_exec::PendingToolRuns::new(
                    super::tool_exec::EventSink::new(self.event_tx.clone(), self.session_id()),
                    tool_manager.clone(),
                    cancel_token.clone(),
                ),
            );

            let (assistant_msg, usage) = {
                self.client
                    .note_prompt_chars(crate::trimming::message_char_count(messages));
                let mut attempt = 0usize;
                loop {
                    attempt += 1;
                    // The `move` closure consumes these, so clone per attempt.
                    let tx = self.event_tx.clone();
                    let sid = self.session_id();
                    let rt_attempt = round_thinking.clone();
                    let pp_tx = self.event_tx.clone();
                    let pp_sid = self.session_id();
                    let ready = pending_tool_runs.ready();
                    match ChatClient::stream_with_messages_arc(
                        &self.client,
                        messages,
                        tool_defs.as_deref(),
                        move |chunk: String, is_thinking: bool| {
                            if is_thinking {
                                rt_attempt.lock().unwrap().push_str(&chunk);
                            }
                            if let Some(ref tx) = tx {
                                if let Ok(g) = tx.lock() {
                                    let _ = g.send(if is_thinking {
                                        crate::types::AppEvent::StreamThinkingChunk { content: chunk, session_id: sid.clone() }
                                    } else {
                                        crate::types::AppEvent::StreamChunk { content: chunk, session_id: sid.clone() }
                                    });
                                }
                            }
                            Ok(())
                        },
                        ready,
                        move |pp: crate::types::PromptProgress| {
                            // Live prompt-processing progress (llama.cpp):
                            // forward to the UI for the status bar PP speed.
                            if let Some(ref tx) = pp_tx {
                                if let Ok(g) = tx.lock() {
                                    let _ = g.send(
                                        crate::types::AppEvent::StreamPromptProgress {
                                            progress: pp,
                                            session_id: pp_sid.clone(),
                                        },
                                    );
                                }
                            }
                        },
                        Some(cancel_token),
                    )
                    .await
                    {
                        Ok((msg, usage)) => break (msg, usage),
                        Err(crate::client::Error::Cancelled) => {
                            pending_tool_runs.abort_all();
                            return Err("Cancelled".to_string());
                        }
                        Err(e) if attempt == 1 => {
                            // Backstop: the trim estimator is a heuristic — if the
                            // server still rejects the request as over-context,
                            // force-trim to 85% of the reported window (in char
                            // units via the measured ratio) and retry once.
                            let Some(ov) = crate::client::parse_context_overflow(&e) else {
                                return Err(format!("LLM call failed: {}", e));
                            };
                            tracing::warn!(
                                "[AGENT] Agent '{}' request exceeded context ({} tokens, n_ctx={}); force-trimming and retrying",
                                self.config.name, ov.n_prompt, ov.n_ctx
                            );
                            let target = self
                                .client
                                .overflow_retry_char_budget(&ov)
                                .saturating_sub(request_overhead_chars(
                                    tool_defs.as_deref(),
                                    messages,
                                ));
                            let removed = self
                                .trimming
                                .trim_messages(messages, target, &self.config.trim_config);
                            self.reconcile_store(messages);
                            tracing::info!(
                                "[AGENT] Agent '{}' force-trim removed {} messages (target_chars={})",
                                self.config.name, removed, target
                            );
                            self.client.note_prompt_chars(crate::trimming::message_char_count(messages));
                            pending_tool_runs.abort_all();
                            pending_tool_runs.clear();
                            continue;
                        }
                        Err(e) => {
                            pending_tool_runs.abort_all();
                            return Err(format!("LLM call failed: {}", e));
                        }
                    }
                }
            };
            // Calibrate the chars/token ratio from the server's real count so
            // subsequent trim budgets track the actual tokenizer.
            self.client.calibrate_from_usage(usage.as_ref());
            self.last_llm_call_at = Instant::now();

            // Commit this round's thinking block to the UI.
            // (Read via `round_thinking` — `rt` was moved into the closure.)
            let round_thinking_str = round_thinking.lock().unwrap().clone();
            if !round_thinking_str.is_empty() {
                self.send_event(crate::types::AppEvent::StreamThinkingComplete {
                    content: round_thinking_str,
                    session_id: self.session_id(),
                });
            }

            let content = assistant_msg.content.clone();
            let tool_calls = assistant_msg.tool_calls.clone();
            let reasoning = assistant_msg.reasoning_content.clone();

            // Record the assistant turn (content + native calls + reasoning).
            // Kept in the throwaway request list AND recorded in the store once.
            let mut assistant_msg_rec = Message {
                role: "assistant".to_string(),
                content: content.clone(),
                timestamp: crate::types::format_timestamp(),
                tool_calls: tool_calls.clone(),
                tool_call_id: None,
                reasoning_content: reasoning.clone(),
                image: None,
            };
            // A tool call whose arguments are not a complete JSON object was
            // cut off by the model's output limit (the stream ended mid-
            // argument). Never store or replay such a call raw: OpenAI-
            // compatible servers parse every tool call in the history on each
            // request and reject the whole request with a 500 when they find
            // an incomplete one. Repair the arguments to `{}` before the turn
            // is recorded anywhere; the execution loop below reports the
            // truncation as a tool result so the model can retry with a
            // smaller payload.
            let truncated_ids: std::collections::HashSet<String> =
                crate::tools::manager::repair_truncated_tool_calls(&mut assistant_msg_rec)
                    .into_iter()
                    .collect();
            if !truncated_ids.is_empty() {
                tracing::warn!(
                    "[AGENT] Agent '{}' had {} truncated tool call(s) (model output limit reached mid-argument): {:?}; arguments repaired before storing",
                    self.config.name,
                    truncated_ids.len(),
                    truncated_ids
                );
            }
            messages.push(assistant_msg_rec.clone());
            self.record_in_store(&assistant_msg_rec);

            // Display-friendly content (think tags stripped if embedded)
            let display_content = crate::client::strip_think_tags(&content);

            // Commit the current round's text to the UI so the stream buffer
            // is flushed between tool-call iterations, and update the token
            // gauge with the server-reported usage. Only for INTERMEDIATE
            // rounds (tool calls present): on the final round the buffer is
            // left intact so StreamComplete commits it exactly once — emitting
            // RoundComplete here too would make the UI append the text twice.
            if tool_calls.as_ref().map(|c| !c.is_empty()).unwrap_or(false) {
                self.send_event(crate::types::AppEvent::StreamRoundComplete {
                    content: display_content.clone(),
                    usage: usage.clone(),
                    session_id: self.session_id(),
                });
            }

            // ── Native tool calls ───────────────────────────────────────
            // Calls the model has already moved past were early-started
            // mid-stream (ready callback above) — collect their results
            // here, in call order. Calls that never got a "moved past"
            // signal (typically the LAST one in the stream) are executed
            // inline, exactly as before.
            if let Some(calls) = &tool_calls {
                if !calls.is_empty() {
                    super::tool_calls::run_native_tool_calls(
                        self,
                        calls,
                        &pending_tool_runs,
                        cancel_token,
                        &truncated_ids,
                        messages,
                        &tool_manager,
                    )
                    .await?;
                    continue;
                }
            }

            // ── Fallback: text-embedded tool calls (non-native models) ──
            if super::tool_calls::run_text_embedded_calls(
                self,
                &tool_defs,
                &display_content,
                cancel_token,
                messages,
                &tool_manager,
            )
            .await?
            {
                continue;
            }

            // ── No more tool calls ──────────────────────────────────────
            if verification_attempts >= MAX_VERIFICATION_ATTEMPTS {
                // S1: the nudge loop was exhausted without a passing verdict —
                // record it as negative evidence.
                self.store_verification_outcome(
                    "gave_up",
                    verification_attempts,
                    &last_failed_judge_reason,
                    &original_request,
                );
                tracing::info!(
                    "[AGENT] Agent '{}' completed ({} verification attempts)",
                    self.config.name,
                    verification_attempts
                );
                self.send_event(crate::types::AppEvent::StreamComplete {
                    content: display_content.clone(),
                    usage: usage.clone(),
                    session_id: self.session_id(),
                });
                break RunOutcome::Completed(display_content);
            }
            verification_attempts += 1;

            let verification_result = self
                .verify_tool_outputs(messages, &original_request, &display_content, cancel_token)
                .await;
            match verification_result {
                Ok(verdict) if verdict.verified => {
                    // S1: a pass that needed a retry is negative evidence for
                    // the first attempt (a first-try pass stays unrecorded —
                    // no noise).
                    if verification_attempts > 1 {
                        self.store_verification_outcome(
                            "verified_after_retry",
                            verification_attempts,
                            &last_failed_judge_reason,
                            &original_request,
                        );
                    }
                    tracing::info!("[AGENT] Agent '{}' completed (verified)", self.config.name);
                    self.send_event(crate::types::AppEvent::StreamComplete {
                        content: display_content.clone(),
                        usage: usage.clone(),
                        session_id: self.session_id(),
                    });
                    break RunOutcome::Completed(display_content);
                }
                Ok(verdict) => {
                    if !verdict.judge_reason.trim().is_empty() {
                        last_failed_judge_reason = verdict.judge_reason.clone();
                    }
                    tracing::warn!(
                        "[AGENT] Agent '{}' verification failed (attempt {}/{}): {}, feeding feedback to LLM",
                        self.config.name,
                        verification_attempts,
                        MAX_VERIFICATION_ATTEMPTS,
                        truncate_chars(&verdict.judge_reason, 200)
                    );
                    messages.push(Message {
                        role: "user".to_string(),
                        content: VERIFICATION_NUDGE.to_string(),
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                        image: None,
                    });
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        "[AGENT] Agent '{}' verification error: {}, proceeding with response",
                        self.config.name,
                        e
                    );
                    self.send_event(crate::types::AppEvent::StreamComplete {
                        content: display_content.clone(),
                        usage: usage.clone(),
                        session_id: self.session_id(),
                    });
                    break RunOutcome::Completed(display_content);
                }
            }
        };

        // I1: record this run's tool-use trajectory for the improver.
        self.run_stats = Self::run_stats_since(messages, run_start_len, verification_attempts);
        Ok(outcome)
    }
}
