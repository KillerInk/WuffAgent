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
    /// The `hand_back` tool was called (this session is a sub-session);
    /// `execute` records a marker, emits `AgentHandBack` with the parent
    /// session id, and ends the turn so the UI can post the task into the
    /// parent session.
    HandBack(crate::agents::types::HandBackRequest),
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
            let manager = self.tool_manager.as_ref();
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
                // S4a: the session-note tool is per-execution (shared control
                // mailbox), so when it is injected it must survive the
                // allowlist filter like handoff/restart.
                if self.config.session_note_enabled && !allowlist.iter().any(|t| t == "session_note")
                {
                    allowlist.push("session_note".to_string());
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

        // M1: tool-use trajectory counters, accumulated as the loop runs.
        // (Re-scanning the request list afterwards with a start-of-run
        // offset is unreliable: the list is trimmed in place mid-run, so on
        // long turns the offset drifted past every recorded call and the
        // metrics line came out all-zero.)
        let mut run_stats = crate::agents::types::RunStats::default();
        // 4c: token cost of this run, accumulated from server-reported usage
        // each round (kept local — not part of RunStats — so the 11 existing
        // RunStats literals stay untouched). Fed to the metrics log below.
        let mut tokens_in = 0u64;
        let mut tokens_out = 0u64;
        // 1e: per-run join key for the metrics + usage stores — one id ties
        // this run's Run line, its Trim lines, and the usage.jsonl
        // UsageEntry lines (readers can join either direction).
        let run_id = format!(
            "{}-{}",
            chrono::Utc::now().timestamp_millis(),
            self.config.name
        );
        let session_id = self.session_id();
        // Stamp the client so every LLM call of this run carries the id;
        // the guard clears it on every exit path (success, error, cancel).
        self.client.set_run_id(Some(&run_id));
        let _run_id_guard = RunIdGuard(&self.client);
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

            // ── S4a: session notes ─────────────────────────────────────────
            // A note queued by the `session_note` tool this round is pinned
            // into the conversation: inserted as an anchored user message
            // right after the system prompt (before the task) via
            // `brief::apply_note` and recorded in the shared store, so it
            // survives this run's trims AND session reloads. Unlike
            // handoff/restart/hand-back this does NOT end the run — the next
            // LLM round simply sees the note. `reanchor_notes` then fixes any
            // note that drifted (e.g. right after a session reload, where the
            // store's append order has the notes at the END of the list), so
            // they sit right after the system prompt in EVERY request.
            // S2: control requests (handoff / restart / hand_back / session
            // note) arrive through ONE mailbox, drained once per round.
            // Priority is the legacy drain order: the note is applied first
            // (it never ends the run), then handoff > restart > hand_back —
            // whichever is pending ends the run.
            let control = self.drain_control();

            if let Some(req) = control.iter().find_map(|r| r.as_session_note()) {
                let note = truncate_note(&req.note);
                if let Some(idx) = crate::trimming::brief::apply_note(messages, &note) {
                    self.record_in_store(&messages[idx]);
                    tracing::info!(
                        "[AGENT] Agent '{}' pinned a session note ({} chars); the run continues",
                        self.config.name,
                        note.chars().count()
                    );
                }
            }
            crate::trimming::brief::reanchor_notes(messages);

            // A pending handoff (written by the `handoff` tool this turn)
            // ends this agent's run: `execute` switches to the target agent.
            if let Some(req) = control.iter().find_map(|r| r.as_handoff()) {
                tracing::info!(
                    "[AGENT] Agent '{}' handoff requested via the handoff tool; ending this agent's turn (to='{}')",
                    self.config.name,
                    req.agent
                );
                // Break (not return) so the run-stats + metrics tail below
                // still records this hop before the engine switches agents.
                break RunOutcome::Handoff(req.clone());
            }

            // A pending restart (written by the `restart` tool this turn, its
            // build already finished) ends this agent's run: `execute` emits
            // RestartRequested so the UI can relaunch the (optionally newly
            // built) binary and resume the session automatically.
            if let Some(req) = control.iter().find_map(|r| r.as_restart()) {
                tracing::info!(
                    "[AGENT] Agent '{}' restart requested via the restart tool; ending this agent's turn (reason='{}')",
                    self.config.name,
                    req.reason
                );
                // Break (not return) so the metrics tail records this run.
                break RunOutcome::Restart(req.clone());
            }

            // A pending hand-back (written by the `hand_back` tool this turn;
            // only sub-sessions get the tool) ends this agent's run: `execute`
            // records a marker, emits AgentHandBack with the parent session
            // id, and ends the turn so the UI can post the task into the
            // parent session.
            if let Some(req) = control.iter().find_map(|r| r.as_hand_back()) {
                tracing::info!(
                    "[AGENT] Agent '{}' hand-back requested via the hand_back tool; ending this agent's turn (task='{}')",
                    self.config.name,
                    req.task
                );
                // Break (not return) so the metrics tail records this run.
                break RunOutcome::HandBack(req.clone());
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
            // Two thresholds (per-agent `TrimConfig` pcts, default 90/50):
            // history grows freely until it crosses the trigger (90% of
            // n_ctx); once it does, we do NOT stop just under the limit —
            // we trim all the way down to the target (50% of n_ctx) so the
            // following rounds have headroom.
            if self.client.n_ctx() > 0 {
                let msg_count = messages.len();
                // The request also carries the tool schemas and attached
                // images, which `message_char_count` does not count — reserve
                // that overhead so the budget covers the ACTUAL request size.
                let overhead_chars = request_overhead_chars(tool_defs.as_deref(), messages);
                let total_chars = crate::trimming::message_char_count(messages) + overhead_chars;
                if msg_count > 4 && total_chars > self.client.trim_trigger_chars() {
                    // Target in char units: the configured target percentage
                    // of n_ctx (default 50) converted to chars via the
                    // client's calibrated chars-per-token ratio, minus the
                    // per-request overhead so messages + overhead together
                    // stay under the target.
                    let target_chars = self.client.trim_target_chars().saturating_sub(overhead_chars);
                    let chars_before = crate::trimming::message_char_count(messages);
                    let (removed, dropped) = self.trimming.trim_messages_detailed(
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
                        // S2 (context-rot signal): how often the model is forced
                        // to drop its own history, and whether the mission brief
                        // re-anchored the state. Best-effort — never breaks the run.
                        let brief_present = messages
                            .iter()
                            .any(crate::trimming::brief::is_brief_message);
                        crate::agents::metrics::record_trim(
                            &self.config.name,
                            chars_before as u64,
                            crate::trimming::message_char_count(messages) as u64,
                            removed as u32,
                            brief_present,
                            false,
                            &run_id,
                        );
                    }
                    // S4b: LLM brief polish (flag-gated, best-effort). The
                    // deterministic brief is already anchored in the list; when
                    // the dropped span is large enough, ONE small LLM call
                    // refines its sections in model quality. The request
                    // carries ONLY the old brief + the dropped span (capped)
                    // — small by construction, cannot overflow. Any failure
                    // keeps the deterministic brief. Runs before
                    // `reconcile_store`, so the polished brief is what the
                    // store/session file get.
                    if self.config.trim_config.llm_brief_polish
                        && crate::trimming::message_char_count(&dropped)
                            >= crate::trimming::brief::BRIEF_POLISH_MIN_DROPPED_CHARS
                    {
                        if let Some(text) = self.polish_brief(messages, &dropped, target_chars).await {
                            tracing::info!(
                                "[AGENT] Agent '{}' brief polished by the LLM ({} chars)",
                                self.config.name,
                                text.chars().count()
                            );
                        }
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

            // 1d: time this round's LLM call (the main-round part of
            // llm_ms; the judge's time is folded in at run end).
            let llm_round_started = Instant::now();
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
                            let chars_before = crate::trimming::message_char_count(messages);
                            let removed = self
                                .trimming
                                .trim_messages(messages, target, &self.config.trim_config);
                            self.reconcile_store(messages);
                            tracing::info!(
                                "[AGENT] Agent '{}' force-trim removed {} messages (target_chars={})",
                                self.config.name, removed, target
                            );
                            // S2 (context-rot signal): the backstop path fired —
                            // frequent `overflow` trims mean the proactive
                            // estimator/trigger is miscalibrated.
                            if removed > 0 {
                                let brief_present = messages
                                    .iter()
                                    .any(crate::trimming::brief::is_brief_message);
                                crate::agents::metrics::record_trim(
                                    &self.config.name,
                                    chars_before as u64,
                                    crate::trimming::message_char_count(messages) as u64,
                                    removed as u32,
                                    brief_present,
                                    true,
                                    &run_id,
                                );
                            }
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
            run_stats.llm_ms += llm_round_started.elapsed().as_millis() as u64;
            // Calibrate the chars/token ratio from the server's real count so
            // subsequent trim budgets track the actual tokenizer.
            self.client.calibrate_from_usage(usage.as_ref());
            // 4c: accumulate this round's server-reported tokens (cost
            // evidence for the metrics log / improver). `usage` is still
            // borrowed here — it is moved into the verify call later.
            if let Some(u) = usage.as_ref() {
                tokens_in = tokens_in.saturating_add(u.prompt_tokens as u64);
                tokens_out = tokens_out.saturating_add(u.completion_tokens as u64);
            }
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
            // mid-stream (ready callback above). The section below starts
            // whatever is still unstarted (typically the LAST call in the
            // stream, which never got a "moved past" signal) in the
            // background too, then collects ALL results in call order —
            // i.e. every tool in the round runs in parallel.
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
                        &mut run_stats,
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
                &pending_tool_runs,
                messages,
                &tool_manager,
                &mut run_stats,
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

        // I1: record this run's tool-use trajectory for the improver;
        // M1: per-agent metrics line (trajectory + terminal verification
        // outcome + wall-clock duration). Every Ok exit — verified,
        // gave-up, handoff, restart, hand-back — funnels through here (the
        // tool-driven exits `break` with their outcome instead of returning,
        // so a run is never recorded zero or skipped). Best-effort, never
        // fails the run.
        run_stats.verification_attempts = verify_state.attempts;
        // 1d: fold the verification judge's time into the LLM bucket —
        // llm_ms is "all wall-clock spent in the LLM" (main rounds + judge).
        run_stats.llm_ms += verify_state.judge_ms;
        // 1b: stamp the run's model + estimated cost (the price table comes
        // from the app config; unknown model = 0.0 "recorded but unpriced").
        run_stats.model = self.client.last_model();
        run_stats.cost_usd = crate::usage::cost_usd(
            &self.model_prices,
            &run_stats.model,
            tokens_in,
            tokens_out,
        );
        self.run_stats = run_stats;
        // 2b: expose this run's token usage (the eval harness reads it for the
        // Eval line's cost record; tokens are loop-local, not in RunStats).
        self.run_tokens_in = tokens_in;
        self.run_tokens_out = tokens_out;
        // 2b: synthetic runs (the eval harness's headless runs) set
        // `metrics_enabled = false` so they don't pollute a profile's
        // real-run metrics; they write an `Eval` line instead (see
        // `run_eval`).
        if self.config.metrics_enabled {
            crate::agents::metrics::record_run(
                &self.config.name,
                &self.run_stats,
                start.elapsed().as_millis() as u64,
                verify_state.final_outcome.unwrap_or(crate::agents::metrics::RunOutcome::None),
                tokens_in,
                tokens_out,
                &run_id,
                &session_id,
            );
        }
        Ok(outcome)
    }

    /// S4b: refine the just-updated mission brief with one small LLM call.
    ///
    /// The request carries ONLY the old brief render + the dropped span
    /// (see `brief::polish_request`) — small by construction, so it cannot
    /// overflow the window. The response is re-validated through the same
    /// render/parse contract as the deterministic path (`parse_polish` +
    /// `enforce_total_cap` + `render`): a malformed answer keeps the
    /// deterministic brief, and the task is never lost (when the model drops
    /// it, the deterministic brief's task is restored). The polished brief
    /// is applied in place (never stacked); returns it on success.
    async fn polish_brief(
        &self,
        messages: &mut Vec<Message>,
        dropped: &[Message],
        target_chars: usize,
    ) -> Option<String> {
        let prev_text = messages
            .iter()
            .find(|m| crate::trimming::brief::is_brief_message(m))?
            .content
            .clone();
        let prev = crate::trimming::brief::from_rendered(&prev_text)?;
        let req = crate::trimming::brief::polish_request(Some(&prev_text), dropped);
        let (out, _usage) = self.client.complete_messages(&req, None).await.ok()?;
        let mut polished = crate::trimming::brief::parse_polish(&out)?;
        // The task is never lost: keep the deterministic brief's task when
        // the model dropped it.
        if polished.task.is_empty() {
            polished.task = prev.task.clone();
        }
        crate::trimming::brief::enforce_total_cap(&mut polished);
        let text = crate::trimming::brief::render(&polished);
        // Same fit-guard as the deterministic insert: a brief that alone
        // does not fit the budget would make the budget unreachable, so the
        // deterministic brief stays in place.
        if text.is_empty() || text.chars().count() >= target_chars {
            return None;
        }
        crate::trimming::brief::apply_brief(messages, &text);
        Some(text)
    }
}

/// S4a: truncate a session-note input to
/// [`crate::trimming::brief::NOTE_INPUT_MAX`] chars (char boundary), marking
/// the cut with "…" — a note is a pointer, not a transcript (the same rule
/// the tool advertises).
fn truncate_note(note: &str) -> String {
    if note.chars().count() <= crate::trimming::brief::NOTE_INPUT_MAX {
        return note.to_string();
    }
    let mut out: String = note.chars().take(crate::trimming::brief::NOTE_INPUT_MAX - 1).collect();
    out.push('…');
    out
}

/// 1e: clears the client's run-id stamp when a run ends on any path
/// (success, error, cancel) — the stamp must never leak into the next
/// agent's calls. The next run re-stamps at its start anyway; the guard
/// keeps the window between runs clean.
struct RunIdGuard<'a>(&'a crate::client::ChatClient);

impl Drop for RunIdGuard<'_> {
    fn drop(&mut self) {
        self.0.set_run_id(None);
    }
}
