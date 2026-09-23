//! Shared LLM tool-call loop (`run_llm_loop`).
//!
//! Split out of agents/agent.rs (A1) and progressively broken down in
//! Phase A: the parallel tool-execution machinery lives in `tool_exec.rs`,
//! the tool-call sections in `tool_calls.rs`, mid-run injections in
//! `inject.rs`, the verify-&-complete section in `verify.rs`, and the entry
//! point + handoff/restart routing in `execute.rs`. This file keeps the loop
//! skeleton: rate limiting, per-request setup, streaming, and context
//! trimming. `RunOutcome`, `request_overhead_chars`, and
//! `ESTIMATED_IMAGE_CHARS` stay here (other modules re-import them via
//! `super::r#loop::`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::Agent;
use crate::client::ChatClient;
use crate::types::Message;

/// Minimum delay between LLM calls to prevent API rate-limiting (500ms).
const LLM_RATE_LIMIT_DELAY: Duration = Duration::from_millis(500);
/// Maximum verification attempts before giving up.
pub(crate) const MAX_VERIFICATION_ATTEMPTS: u32 = 2;
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
        // S1: verification bookkeeping for this run (attempt counter +
        // last NEEDS_FIX reason for the outcome memory).
        let mut verify_state = super::verify::VerificationState::new();
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
            self.drain_injections(messages, &mut original_request);

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
            match self
                .verify_or_complete(
                    messages,
                    display_content,
                    usage,
                    &original_request,
                    cancel_token,
                    &mut verify_state,
                )
                .await?
            {
                Some(outcome) => break outcome,
                None => continue,
            }
        };

        // I1: record this run's tool-use trajectory for the improver.
        self.run_stats =
            Self::run_stats_since(messages, run_start_len, verify_state.attempts);
        Ok(outcome)
    }
}
