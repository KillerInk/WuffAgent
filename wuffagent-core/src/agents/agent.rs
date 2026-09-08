use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing;

use super::config::AgentConfig;
use super::invocation_registry::AgentInvocationRegistry;
use super::LlmClient;
use crate::client::ChatClient;
use super::types::AgentId;
use crate::tools::ToolManager;
use crate::types::Message;
use crate::trimming::ContextTrimming;

/// Minimum delay between LLM calls to prevent API rate-limiting (500ms).
const LLM_RATE_LIMIT_DELAY: Duration = Duration::from_millis(500);

/// Maximum verification attempts before giving up.
const MAX_VERIFICATION_ATTEMPTS: u32 = 2;

/// Minimum number of messages required before trimming is attempted.
#[allow(dead_code)]
const MIN_MESSAGES_FOR_TRIM: usize = 4;

/// Maximum characters of tool output to include in summary.
const TOOL_OUTPUT_SUMMARY_CHARS: usize = 200;

/// Maximum characters of user request for verification prompts.
const REQUEST_TRUNCATION_CHARS: usize = 500;

/// Maximum characters of the assistant's final response for verification
/// prompts. The response is model-generated (not user-controlled), so a
/// generous budget is safe — this only bounds the judge call's prompt size.
const RESPONSE_TRUNCATION_CHARS: usize = 2000;

/// Timeout in seconds for verification LLM calls.
const VERIFICATION_TIMEOUT_SECS: u64 = 60;

/// System prompt for response verification.
///
/// The judge grades the assistant's *response* against the tool outputs it
/// relied on — not the raw tool outputs alone. The old wording ("do the tool
/// outputs answer the request?") failed legitimate answers: intermediate
/// outputs (file dumps, search results) rarely contain the full answer by
/// themselves, so the judge returned NEEDS_FIX and the loop wasted an extra
/// LLM round re-asking a model that had already answered correctly.
static VERIFICATION_SYSTEM_PROMPT: &str =
    "You are verifying whether an assistant's response fully satisfies the user's request, \
     using the tool outputs it relied on as evidence. \
     Respond with exactly 'VERIFIED' if the response is correct, complete, and consistent with the tool outputs. \
     Respond with 'NEEDS_FIX' followed by a brief explanation ONLY if the response is factually wrong, \
     incomplete, or contradicts the tool outputs. \
     Do NOT reply NEEDS_FIX merely because the tool outputs alone do not spell out the full answer — \
     the response itself is what you are grading.";

/// A configurable agent that runs an LLM loop with tool calls.
///
/// Each agent has its own system prompt, allowed tools, and can invoke
/// other agents via the agent_call tool.
#[derive(Clone)]
pub struct Agent {
    id: AgentId,
    config: AgentConfig,
    llm_client: Arc<dyn LlmClient>,
    tool_manager: Arc<Mutex<ToolManager>>,
    invocation_registry: Arc<AgentInvocationRegistry>,
    event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
    /// Chat client used for streaming (native tool-call) requests.
    client: Arc<ChatClient>,
    /// Memory manager for persistent context.
    memory: Option<Arc<crate::memory::MemoryManager>>,
    /// Stored messages from the last execution for memory extraction.
    messages: Vec<Message>,
    /// Timestamp of the last LLM call, used for rate limiting between iterations.
    last_llm_call_at: Instant,
    /// Session ID for this agent's persistent conversation.
    agent_session_id: Option<String>,
    /// Directory where this agent's session files are stored.
    agent_session_dir: PathBuf,
    /// Centralized trimming engine.
    trimming: ContextTrimming,
}

impl Agent {
    /// Create a new agent from config.
    pub fn new(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<Mutex<ToolManager>>,
        invocation_registry: Arc<AgentInvocationRegistry>,
        event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
        client: Arc<ChatClient>,
        memory: Option<Arc<crate::memory::MemoryManager>>,
        agent_session_id: Option<String>,
        agent_session_dir: PathBuf,
    ) -> Self {
        // Apply the agent's per-agent reasoning effort: give it its own
        // client clone with the effort set. Off = inherit the global
        // client setting (no override).
        let client = if config.reasoning_effort != crate::types::ReasoningEffort::Off {
            let mut c = (*client).clone();
            c.set_reasoning_effort(config.reasoning_effort);
            Arc::new(c)
        } else {
            client
        };
        // Give this agent its own tool manager whose `shell` honors the agent's
        // shell config (allowlist/enabled/timeout), instead of sharing the global
        // allow-all shell. All other tools are shared. This is what makes an
        // agent's `shell` respect its per-agent restrictions on both the chat and
        // /plan paths.
        let tool_manager = {
            let shared = tool_manager.lock().unwrap();
            let tm = shared.with_shell_config(config.get_shell_config());
            // Enforce the agent's `can_invoke` list on `agent_call`: a non-empty
            // list restricts the tool to those targets, an empty list removes
            // the tool from the schema entirely so the agent cannot delegate.
            let tm = if config.can_invoke.is_empty() {
                tm.with_agent_call_allowlist(&invocation_registry, None)
            } else {
                tm.with_agent_call_allowlist(&invocation_registry, Some(&config.can_invoke))
            };
            Arc::new(Mutex::new(tm))
        };
        Self {
            id: AgentId::generate(),
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            event_tx,
            last_llm_call_at: Instant::now(),
            client,
            memory,
            messages: Vec::new(),
            agent_session_id,
            agent_session_dir,
            trimming: ContextTrimming::new(),
        }
    }

    /// Build the throwaway request list for the current turn: the fresh system
    /// prompt followed by a snapshot of the shared store.
    ///
    /// The system prompt is rebuilt each run (current memory context) and lives
    /// ONLY in outgoing requests — it is never written to the store. The user
    /// message for this turn is already in the shared store (appended at turn
    /// start in `execute`), so it is included here via the store snapshot.
    ///
    /// This list is a request body, not history: it may contain the system
    /// message and verification nudge, neither of which is persisted.
    pub fn build_initial_messages(&self, _task: &str) -> Vec<Message> {
        let now = crate::types::format_timestamp();
        let mut messages: Vec<Message> = Vec::new();

        // Start with the fresh system prompt (request-only, never stored).
        messages.push(Message {
            role: "system".to_string(),
            content: self.build_system_prompt(),
            timestamp: now,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        // Snapshot the shared store, skipping anything that must never appear in a
        // request body (stray system messages, empty assistant placeholders).
        {
            let conv = self.client.conversation();
            let guard = conv.lock().unwrap();
            for msg in guard.iter() {
                if msg.role == "system" {
                    continue;
                }
                if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
                    continue;
                }
                messages.push(msg.clone());
            }
        }

        messages
    }

    /// Create an agent from config with an empty tool manager.
    pub fn from_config(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        invocation_registry: Arc<AgentInvocationRegistry>,
        client: Arc<ChatClient>,
        memory: Option<Arc<crate::memory::MemoryManager>>,
    ) -> Self {
        let tool_registry = Arc::new(crate::tools::registry::ToolRegistry::new(
            vec![],
            Arc::new(crate::tools::types::TracingToolLogger),
        ));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        Self::new(
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            None,
            client,
            memory,
            None,
            PathBuf::new(),
        )
    }

    fn send_event(&self, event: crate::types::AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }

    /// The session ID to stamp on events (falls back to empty when unset).
    fn session_id(&self) -> String {
        self.agent_session_id.clone().unwrap_or_default()
    }

    /// Get the messages from the last execution for memory extraction.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Execute a request with this agent.
    pub async fn execute(
        &mut self,
        request: &str,
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
            });
        }

        // Throwaway request list: fresh system prompt + a snapshot of the store
        // (which already contains the user message from the step above).
        let mut messages = self.build_initial_messages(request);

        self.send_event(crate::types::AppEvent::AgentChainStarted {
            agent_name: self.config.name.clone(),
            depth: 0,
            session_id: self.session_id(),
        });

        let result = self.run_llm_loop(&mut messages, cancel_token).await;

        // Keep a clean copy (store snapshot, no system prompts) for memory
        // extraction. Assistant/tool messages were already recorded to the store
        // as they were generated inside run_llm_loop, so there is no sync-back.
        self.messages = self.client.conversation().lock().unwrap().clone();

        // Persistence is owned by the UI (it saves on StreamComplete, for both the
        // agent and non-agent paths), so the agent does not write the session file
        // itself. This keeps a single writer per store and avoids a redundant save.

        match &result {
            Ok(response) => {
                self.send_event(crate::types::AppEvent::AgentChainCompleted {
                    agent_name: self.config.name.clone(),
                    result: response.clone(),
                    depth: 0,
                    session_id: self.session_id(),
                });
            }
            Err(e) => {
                self.send_event(crate::types::AppEvent::AgentChainError {
                    agent_name: self.config.name.clone(),
                    error: e.clone(),
                    depth: 0,
                    session_id: self.session_id(),
                });
            }
        }

        result
    }

    /// Build the system prompt for this agent.
    fn build_system_prompt(&self) -> String {
        let mut prompt = if self.config.system_prompt.is_empty() {
            format!("You are the '{}' agent. {}", self.config.name, self.config.description)
        } else {
            self.config.system_prompt.clone()
        };

        // Add available agents for delegation. Only list the agents this agent
        // is allowed to invoke (`can_invoke`); when the list is empty the
        // agent_call tool is removed from its schema entirely, so no
        // delegation hint is added.
        if !self.config.can_invoke.is_empty() {
            prompt.push_str("\n\nYou can delegate tasks to other agents using the agent_call tool. You may only invoke these agents: ");
            prompt.push_str(&self.config.can_invoke.join(", "));
            prompt.push_str(
                "\nPrefer your own tools when a task can be done in a single step. \
                 Delegate only when the sub-task needs another agent's specialization \
                 or benefits from an isolated context — each delegation spawns a full \
                 sub-conversation and is more expensive than a direct tool call.",
            );
        }

        // Inject relevant memories
        if let Some(memory) = &self.memory {
            let memory_block = memory.build_context_block("");
            if !memory_block.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&memory_block);
            }
        }

        // Note: system prompt caching would require &'mut self, which conflicts
        // with the LLM loop. The prompt is cheap to rebuild (~100ns).
        prompt
    }

    /// Record a generated message in the shared store, exactly once.
    ///
    /// System messages (the per-run system prompt and the verification retry
    /// nudge) belong only in outgoing requests and are never persisted, so they
    /// are skipped here. This mirrors the client's non-agent path, where the
    /// user and assistant messages are written to the shared conversation as
    /// they happen and the system prompt is never stored.
    fn record_in_store(&self, msg: &Message) {
        // System messages are request-only and never persisted.
        if msg.role == "system" {
            return;
        }
        // Empty assistant placeholders (no content, no tool calls) are a
        // streaming artifact and are never persisted.
        if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
            return;
        }
        let conv = self.client.conversation();
        conv.lock().unwrap().push(msg.clone());
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
    async fn run_llm_loop(
        &mut self,
        messages: &mut Vec<Message>,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        let mut verification_attempts = 0u32;
        let start = Instant::now();

        // Tool definitions for native function calling, filtered per-agent.
        let tool_defs: Option<Vec<crate::tools::ToolDefinition>> = {
            let manager = self.tool_manager.lock().unwrap();
            let mgr = if self.config.allowed_tools.is_empty() {
                manager.clone()
            } else {
                manager.with_allowlist(&self.config.allowed_tools)
            };
            let defs = mgr.get_tool_definitions();
            if defs.is_empty() { None } else { Some(defs) }
        };

        loop {
            if cancel_token.is_cancelled() {
                return Err("Cancelled".to_string());
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
            if self.client.n_ctx() > 0 {
                // Budget in char units: 90% of n_ctx tokens converted to chars
                // via the client's calibrated chars-per-token ratio.
                let target_chars = self.client.trim_budget_chars();
                let msg_count = messages.len();
                if msg_count > 4 {
                    // Trim self.messages directly: stream_with_messages_arc writes to a
                    // throwaway local_conv and never touches self.client.conversation,
                    // so trimming the client's conversation would be a no-op.
                    let removed = self.trimming
                        .trim_messages(messages, target_chars, &self.config.trim_config);
                    if removed > 0 {
                        tracing::info!(
                            "[AGENT] Agent '{}' trimmed {} messages (n_ctx={}, target_chars={})",
                            self.config.name, removed, self.client.n_ctx(), target_chars
                        );
                    }
                    // Post-trim verification: the trim already truncates the largest
                    // message as a fallback; log if we're still over budget.
                    let post_trim_total = crate::trimming::message_char_count(messages);
                    if post_trim_total > target_chars {
                        tracing::warn!(
                            "[AGENT] Post-trim count {} > target {}",
                            post_trim_total, target_chars
                        );
                    }
                }
            }

            // ── LLM call (streaming with native tools) ──────────────────
            // The callback must be 'static, so it captures cloned Arcs rather
            // than `self`.
            let round_thinking = Arc::new(Mutex::new(String::new()));

            let (assistant_msg, usage) = {
                self.client.note_prompt_chars(crate::trimming::message_char_count(messages));
                let mut attempt = 0usize;
                loop {
                    attempt += 1;
                    // The `move` closure consumes these, so clone per attempt.
                    let tx = self.event_tx.clone();
                    let sid = self.session_id();
                    let rt_attempt = round_thinking.clone();
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
                        Some(cancel_token),
                    )
                    .await
                    {
                        Ok((msg, usage)) => break (msg, usage),
                        Err(crate::client::Error::Cancelled) => return Err("Cancelled".to_string()),
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
                            let target = self.client.overflow_retry_char_budget(&ov);
                            let removed = self
                                .trimming
                                .trim_messages(messages, target, &self.config.trim_config);
                            tracing::info!(
                                "[AGENT] Agent '{}' force-trim removed {} messages (target_chars={})",
                                self.config.name, removed, target
                            );
                            self.client.note_prompt_chars(crate::trimming::message_char_count(messages));
                            continue;
                        }
                        Err(e) => return Err(format!("LLM call failed: {}", e)),
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
            let assistant_msg_rec = Message {
                role: "assistant".to_string(),
                content: content.clone(),
                timestamp: crate::types::format_timestamp(),
                tool_calls: tool_calls.clone(),
                tool_call_id: None,
                reasoning_content: reasoning.clone(),
            };
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
            if let Some(calls) = &tool_calls {
                if !calls.is_empty() {
                    for call in calls {
                        if cancel_token.is_cancelled() {
                            return Err("Cancelled".to_string());
                        }
                        self.send_event(crate::types::AppEvent::ToolCallStart {
                            tool_name: call.function.name.clone(),
                            call_id: call.id.clone(),
                            session_id: self.session_id(),
                        });
                        let params = match crate::tools::manager::parse_tool_args(&call.function.arguments) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("[AGENT] Bad args for '{}': {}", call.function.name, e);
                                self.send_event(crate::types::AppEvent::ToolCallError {
                                    tool_name: call.function.name.clone(),
                                    call_id: call.id.clone(),
                                    error: e.clone(),
                                    session_id: self.session_id(),
                                });
                                let bad_args_msg = Message {
                                    role: "tool".to_string(),
                                    content: format!("Error: {}", e),
                                    timestamp: crate::types::format_timestamp(),
                                    tool_calls: None,
                                    tool_call_id: Some(call.id.clone()),
                                    reasoning_content: None,
                                };
                                messages.push(bad_args_msg.clone());
                                self.record_in_store(&bad_args_msg);
                                continue;
                            }
                        };
                        let manager = self.tool_manager.lock().unwrap().clone();
                        let tool_result = manager.execute(&call.function.name, params).await;
                        // Tools must always return *something*: an empty result
                        // string becomes an empty `role: "tool"` message, which
                        // the model/server rejects.
                        let result_str = match tool_result {
                            Ok(output) => {
                                let s = format!("{}", output);
                                if s.trim().is_empty() {
                                    "(no output)".to_string()
                                } else {
                                    s
                                }
                            }
                            Err(e) => {
                                tracing::warn!("[AGENT] Tool '{}' failed: {}", call.function.name, e);
                                format!("Error: {}", e)
                            }
                        };
                        self.send_event(crate::types::AppEvent::ToolCallComplete {
                            tool_name: call.function.name.clone(),
                            call_id: call.id.clone(),
                            result: result_str.clone(),
                            session_id: self.session_id(),
                        });
                        let tool_msg = Message {
                            role: "tool".to_string(),
                            content: result_str,
                            timestamp: crate::types::format_timestamp(),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                            reasoning_content: None,
                        };
                        messages.push(tool_msg.clone());
                        self.record_in_store(&tool_msg);
                    }
                    continue;
                }
            }

            // ── Fallback: text-embedded tool calls (non-native models) ──
            if tool_defs.as_ref().map(|d| d.is_empty()).unwrap_or(false) {
                // No tools offered — skip text parsing entirely.
            } else {
                let mut embedded = Vec::new();
                if let Some(bash_calls) = self.extract_bash_as_tool_calls(&display_content) {
                    embedded.extend(bash_calls);
                }
                if embedded.is_empty() {
                    if let Some(json_calls) = self.parse_tool_calls(&display_content) {
                        embedded.extend(json_calls);
                    }
                }
                if !embedded.is_empty() {
                    for call in &embedded {
                        if cancel_token.is_cancelled() {
                            return Err("Cancelled".to_string());
                        }
                        self.send_event(crate::types::AppEvent::ToolCallStart {
                            tool_name: call.function.name.clone(),
                            call_id: call.id.clone(),
                            session_id: self.session_id(),
                        });
                        let params = match crate::tools::manager::parse_tool_args(&call.function.arguments) {
                            Ok(p) => p,
                            Err(e) => {
                                self.send_event(crate::types::AppEvent::ToolCallError {
                                    tool_name: call.function.name.clone(),
                                    call_id: call.id.clone(),
                                    error: e.clone(),
                                    session_id: self.session_id(),
                                });
                                continue;
                            }
                        };
                        let manager = self.tool_manager.lock().unwrap().clone();
                        let tool_result = manager.execute(&call.function.name, params).await;
                        let result_str = match tool_result {
                            Ok(output) => {
                                let s = format!("{}", output);
                                if s.trim().is_empty() { "(no output)".to_string() } else { s }
                            }
                            Err(e) => format!("Error: {}", e),
                        };
                        self.send_event(crate::types::AppEvent::ToolCallComplete {
                            tool_name: call.function.name.clone(),
                            call_id: call.id.clone(),
                            result: result_str.clone(),
                            session_id: self.session_id(),
                        });
                        // History entry in API-native shape (id links the result).
                        let fb_assistant = Message {
                            role: "assistant".to_string(),
                            content: String::new(),
                            timestamp: crate::types::format_timestamp(),
                            tool_calls: Some(vec![crate::types::ToolCall {
                                id: call.id.clone(),
                                call_type: call._call_type.clone(),
                                function: crate::types::ToolFunction {
                                    name: call.function.name.clone(),
                                    arguments: call.function.arguments.clone(),
                                },
                            }]),
                            tool_call_id: None,
                            reasoning_content: None,
                        };
                        messages.push(fb_assistant.clone());
                        self.record_in_store(&fb_assistant);
                        let fb_tool = Message {
                            role: "tool".to_string(),
                            content: result_str,
                            timestamp: crate::types::format_timestamp(),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                            reasoning_content: None,
                        };
                        messages.push(fb_tool.clone());
                        self.record_in_store(&fb_tool);
                    }
                    continue;
                }
            }

            // ── No more tool calls ──────────────────────────────────────
            if verification_attempts >= MAX_VERIFICATION_ATTEMPTS {
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
                return Ok(display_content);
            }
            verification_attempts += 1;

            let original_request = self.extract_original_request(messages);
            let verification_result = self.verify_tool_outputs(messages, &original_request, &display_content, cancel_token).await;
            match verification_result {
                Ok(true) => {
                    tracing::info!(
                        "[AGENT] Agent '{}' completed (verified)",
                        self.config.name
                    );
                    self.send_event(crate::types::AppEvent::StreamComplete {
                        content: display_content.clone(),
                        usage: usage.clone(),
                        session_id: self.session_id(),
                    });
                    return Ok(display_content);
                }
                Ok(false) => {
                    tracing::warn!(
                        "[AGENT] Agent '{}' verification failed (attempt {}/{}), feeding feedback to LLM",
                        self.config.name,
                        verification_attempts,
                        MAX_VERIFICATION_ATTEMPTS
                    );
                    messages.push(Message {
                        role: "user".to_string(),
                        content: "Your previous response did not fully satisfy the request. Improve it based on the tool outputs, or correct your tool calls and try again.".to_string(),
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
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
                    return Ok(display_content);
                }
            }
        }
    }

    /// Extract the current turn's user request from the message history.
    ///
    /// Uses the LAST user message: the request the current turn's response is
    /// answering. In a multi-turn session the first user message is stale and
    /// would make the judge grade the current answer against the wrong request.
    fn extract_original_request(&self, messages: &[Message]) -> String {
        messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    /// Verify that the assistant's final response satisfies the user's request.
    ///
    /// The judge LLM sees three things: the original user request (truncated,
    /// to blunt prompt injection), summaries of the tool outputs, and the
    /// assistant's final response — and it grades the RESPONSE against the
    /// outputs as evidence. (The pre-fix prompt asked only "do the tool
    /// outputs answer the request?", which failed correct answers:
    /// intermediate outputs rarely contain the full answer by themselves.)
    async fn verify_tool_outputs(
        &self,
        messages: &[Message],
        original_request: &str,
        final_response: &str,
        cancel_token: &CancellationToken,
    ) -> Result<bool, String> {
        // Scope the evidence to the current turn: only tool outputs produced
        // after the most recent user message count as evidence for this
        // turn's response. In a multi-turn session the full history holds stale
        // tool results from earlier turns; feeding those to the judge made it
        // return NEEDS_FIX for a perfectly complete answer to the current
        // request, which then re-asked the model after it had already finished.
        let turn_start = messages
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(0);
        let tool_outputs: Vec<String> = messages
            .iter()
            .skip(turn_start)
            .filter(|m| m.role == "tool")
            .map(|m| m.content.clone())
            .collect();

        // Skip verification if no tool calls were made this turn.
        if tool_outputs.is_empty() {
            return Ok(true);
        }
        let recent_tool_summary: String = tool_outputs
            .iter()
            .map(|o| o.chars().take(TOOL_OUTPUT_SUMMARY_CHARS).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        let verification_messages = vec![
            Message {
                role: "system".to_string(),
                content: VERIFICATION_SYSTEM_PROMPT.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: "user".to_string(),
                // Truncate the original request to avoid prompt injection
                // via oversized or adversarially crafted messages.
                content: format!(
                    "User request (truncated to {} chars):\n{}\n\nRecent tool outputs ({} chars each, truncated):\n{}\n\nAssistant response (truncated to {} chars):\n{}\n\nDoes the assistant response fully satisfy the user's request?",
                    REQUEST_TRUNCATION_CHARS,
                    &original_request.chars().take(REQUEST_TRUNCATION_CHARS).collect::<String>(),
                    TOOL_OUTPUT_SUMMARY_CHARS,
                    recent_tool_summary,
                    RESPONSE_TRUNCATION_CHARS,
                    &final_response.chars().take(RESPONSE_TRUNCATION_CHARS).collect::<String>()
                ),
                timestamp: crate::types::format_timestamp(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];

        // Check cancellation before making the LLM call.
        if cancel_token.is_cancelled() {
            return Err("Verification cancelled".to_string());
        }

        let response = match tokio::time::timeout(
            std::time::Duration::from_secs(VERIFICATION_TIMEOUT_SECS),
            async {
                // Propagate cancellation during the LLM call.
                let cancel_clone = cancel_token.clone();
                tokio::select! {
                    result = self.llm_client.complete(&verification_messages) => result,
                    _ = cancel_clone.cancelled() => Err("Verification cancelled".to_string()),
                }
            }
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(format!("Verification LLM call failed: {}", e)),
            Err(_) => return Err("Verification LLM call timed out".to_string()),
        };

        // Robust verification: check NEEDS_FIX first (takes precedence),
        // then check if the response is clearly affirmative.
        let response_upper = response.to_uppercase();
        if response_upper.contains("NEEDS_FIX")
            || response_upper.contains("NOT SATISFIED")
            || response_upper.contains("INCORRECT")
            || response_upper.contains("INCOMPLETE")
        {
            Ok(false)
        } else if response_upper.trim() == "VERIFIED"
            || response_upper.contains("VERIFIED")
        {
            Ok(true)
        } else {
            // Default to verified if unclear — better to continue than to
            // abort a successful execution on an ambiguous LLM response.
            Ok(true)
        }
    }

    /// Parse tool calls from an LLM response.
    /// Uses a bracket-aware parser that tracks both `[`/`]` and `{`/`}`
    /// to correctly handle nested JSON structures.
    fn parse_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        // First try parsing the entire response as JSON directly.
        if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(response) {
            return Some(calls);
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(arr) = obj.get("tool_calls").and_then(|v| v.as_array()) {
                let mut calls: Vec<ToolCall> = Vec::new();
                for v in arr {
                    if let Some(func) = v.get("function") {
                        if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                            if let Some(args_val) = func.get("arguments") {
                                let args_str = match args_val {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                calls.push(ToolCall {
                                    id: v.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                                    _call_type: "function".to_string(),
                                    function: crate::agents::agent::ToolFunction {
                                        name: name.to_string(),
                                        arguments: args_str,
                                    },
                                });
                            }
                        }
                    }
                }
                if !calls.is_empty() {
                    return Some(calls);
                }
            }
        }

        // Fallback: bracket-aware extraction that tracks both [] and {} depth.
        let mut regions: Vec<(usize, usize)> = Vec::new();
        let mut bracket_depth = 0i32;
        let mut brace_depth = 0i32;
        let mut start: Option<usize> = None;
        let mut start_type: Option<char> = None; // '[' or '{'
        let mut in_string = false;
        let mut escape = false;

        for (i, ch) in response.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' if in_string => {
                    escape = true;
                }
                '"' => {
                    in_string = !in_string;
                }
                c if !in_string && (c == '[' || c == '{') => {
                    if bracket_depth == 0 && brace_depth == 0 {
                        start = Some(i);
                        start_type = Some(c);
                    }
                    if c == '[' { bracket_depth += 1; }
                    if c == '{' { brace_depth += 1; }
                }
                c if !in_string && (c == ']' || c == '}') => {
                    if c == ']' { bracket_depth -= 1; }
                    if c == '}' { brace_depth -= 1; }
                    // Only close a region if we're closing the matching depth-0 opener.
                    if bracket_depth < 0 { bracket_depth = 0; }
                    if brace_depth < 0 { brace_depth = 0; }
                    if bracket_depth == 0 && brace_depth == 0 {
                        if let Some(s) = start {
                            if start_type == Some('[') {
                                regions.push((s, i + 1));
                            }
                            start = None;
                            start_type = None;
                        }
                    }
                }
                _ => {}
            }
        }

        for (s, e) in &regions {
            if e > s {
                let json_str = &response[*s..*e];
                if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                    return Some(calls);
                }
            }
        }

        // Fallback: look for JSON inside markdown code blocks.
        if let Some(start) = response.find("```") {
            let rest = &response[start + 3..];
            if let Some(end) = rest.find("```") {
                let json_str = &rest[..end];
                if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                    return Some(calls);
                }
            }
        }

        None
    }

    /// Check if the LLM response is a routing JSON object and extract agent+task.
    fn parse_routing_object(&self, response: &str) -> Option<(String, String)> {
        let candidate: Option<(String, String)> =
            serde_json::from_str::<serde_json::Value>(response)
                .ok()
                .and_then(|value| {
                    let agent = value.get("agent")?.as_str()?;
                    let task = value
                        .get("task")
                        .or_else(|| value.get("request"))?
                        .as_str()?;
                    Some((agent.to_string(), task.to_string()))
                })
                .or_else(|| {
                    let start = response.find("```")?;
                    let rest = &response[start + 3..];
                    let end = rest.find("```")?;
                    let json_str = &rest[..end];
                    let value = serde_json::from_str::<serde_json::Value>(json_str).ok()?;
                    let agent = value.get("agent")?.as_str()?;
                    let task = value
                        .get("task")
                        .or_else(|| value.get("request"))?
                        .as_str()?;
                    Some((agent.to_string(), task.to_string()))
                });

        let (agent, task) = candidate?;

        let is_known = self.invocation_registry.has(&agent);
        if !is_known {
            tracing::debug!(
                "[AGENT] Routing candidate agent '{}' is not a known agent; treating response as a normal answer",
                agent
            );
            return None;
        }

        Some((agent, task))
    }

    /// Extract bash/code blocks from LLM responses and convert them to the
    /// named file tools (read_file, list_dir, search_files, file_ops, ...).
    fn extract_bash_as_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        let mut calls = Vec::new();
        let mut id_counter = 0u32;

        let mut rest = response;
        while let Some(start) = rest.find("```") {
            rest = &rest[start + 3..];
            if let Some(end) = rest.find("```") {
                let block = &rest[..end];
                rest = &rest[end + 3..];

                if block.trim().starts_with('{') || block.trim().starts_with('[') {
                    continue;
                }

                let lines: Vec<&str> = block.lines().collect();
                let cmd_line = if lines.len() > 1 && (lines[0] == "bash" || lines[0] == "sh" || lines[0] == "shell") {
                    lines[1..].join("\n").trim().to_string()
                } else {
                    block.trim().to_string()
                };

                if cmd_line.is_empty() {
                    continue;
                }

                let raw_parts: Vec<&str> = cmd_line.split_whitespace().collect();
                if raw_parts.is_empty() {
                    continue;
                }
                let args_parts: Vec<&str> = raw_parts
                    .iter()
                    .skip(1)
                    .filter(|p| !p.starts_with('-'))
                    .copied()
                    .collect();

                // Emit a single tool call with the given name and JSON arguments.
                let mut emit_tool = |name: &str, args: serde_json::Value| {
                    calls.push(ToolCall {
                        id: format!("call_{}", id_counter),
                        _call_type: "function".to_string(),
                        function: ToolFunction {
                            name: name.to_string(),
                            arguments: serde_json::to_string(&args).unwrap_or_default(),
                        },
                    });
                    id_counter += 1;
                };

                let cmd = raw_parts[0];
                match cmd {
                    "ls" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        emit_tool("list_dir", serde_json::json!({ "path": path }));
                    }
                    "cat" => {
                        for path in &args_parts {
                            emit_tool("read_file", serde_json::json!({ "path": path }));
                        }
                    }
                    "find" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        emit_tool("search_files", serde_json::json!({ "path": path, "pattern": "*" }));
                    }
                    "pwd" => {
                        emit_tool("file_ops", serde_json::json!({ "action": "file_info", "path": "." }));
                    }
                    "mkdir" => {
                        if let Some(path) = args_parts.first() {
                            let recursive = raw_parts.iter().any(|p| *p == "-p" || *p == "--parents");
                            emit_tool("file_ops", serde_json::json!({ "action": "mkdir", "path": path, "recursive": recursive }));
                        }
                    }
                    "rm" => {
                        for path in &args_parts {
                            emit_tool("file_ops", serde_json::json!({ "action": "delete", "path": path }));
                        }
                    }
                    "cp" => {
                        if args_parts.len() >= 2 {
                            emit_tool("file_ops", serde_json::json!({ "action": "copy", "src": args_parts[0], "dest": args_parts[1] }));
                        }
                    }
                    "mv" => {
                        if args_parts.len() >= 2 {
                            emit_tool("file_ops", serde_json::json!({ "action": "move", "src": args_parts[0], "dest": args_parts[1] }));
                        }
                    }
                    "head" | "tail" => {
                        if let Some(path) = args_parts.last() {
                            emit_tool("read_file", serde_json::json!({ "path": path }));
                        }
                    }
                    _ => continue,
                }
            }
        }

        if calls.is_empty() {
            None
        } else {
            Some(calls)
        }
    }
}

/// A tool call from an LLM response.
#[derive(Clone, Debug, serde::Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    _call_type: String,
    function: ToolFunction,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ToolFunction {
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::types::TracingToolLogger;
    use super::super::traits::{AgentError, AgentInvocation};
    use super::super::types::{AgentMetadata, AgentResult, AgentType, TaskStatus};

    fn make_agent(name: &str) -> Agent {
        let config = AgentConfig {
            name: name.to_string(),
            ..Default::default()
        };
        let llm_client = Arc::new(NoopLlm);
        let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        let invocation_registry = Arc::new(AgentInvocationRegistry::new());
        let client = Arc::new(ChatClient::new("http://localhost:1"));
        Agent::new(config, llm_client, tool_manager, invocation_registry, None, client, None, None, PathBuf::new())
    }

    struct NoopLlm;
    #[async_trait::async_trait]
    impl LlmClient for NoopLlm {
        async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
            Ok(String::new())
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    #[test]
    fn test_agent_creation() {
        let agent = make_agent("test");
        assert_eq!(agent.config.name, "test");
        assert!(!agent.id.to_string().is_empty());
    }

    #[test]
    fn test_system_prompt_omits_delegation_when_can_invoke_empty() {
        let agent = make_agent("test");
        let prompt = agent.build_system_prompt();
        assert!(
            !prompt.contains("agent_call"),
            "prompt should not mention agent_call when can_invoke is empty: {}",
            prompt
        );
    }

    #[test]
    fn test_system_prompt_lists_only_allowed_agents_and_hint() {
        let mut config = AgentConfig::default();
        config.name = "generalist".to_string();
        config.can_invoke = vec!["researcher".to_string(), "coder".to_string()];
        let llm_client = Arc::new(NoopLlm);
        let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        let invocation_registry = Arc::new(AgentInvocationRegistry::new());
        let client = Arc::new(ChatClient::new("http://localhost:1"));
        let agent = Agent::new(
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            None,
            client,
            None,
            None,
            PathBuf::new(),
        );
        let prompt = agent.build_system_prompt();
        assert!(prompt.contains("researcher, coder"), "prompt should list allowed agents: {}", prompt);
        assert!(
            prompt.contains("Prefer your own tools"),
            "prompt should include the delegation policy hint: {}",
            prompt
        );
    }

    #[test]
    fn test_agent_per_agent_reasoning_effort() {
        let llm_client = Arc::new(NoopLlm);
        let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        let invocation_registry = Arc::new(AgentInvocationRegistry::new());
        // Global client set to Medium.
        let global_client = Arc::new({
            let mut c = ChatClient::new("http://localhost:1");
            c.set_reasoning_effort(crate::types::ReasoningEffort::Medium);
            c
        });

        // Agent with High: gets its own client clone with High.
        let mut config = AgentConfig::default();
        config.name = "researcher".to_string();
        config.reasoning_effort = crate::types::ReasoningEffort::High;
        let agent = Agent::new(
            config,
            llm_client.clone(),
            tool_manager.clone(),
            invocation_registry.clone(),
            None,
            global_client.clone(),
            None,
            None,
            PathBuf::new(),
        );
        assert_eq!(agent.client.reasoning_effort(), crate::types::ReasoningEffort::High);
        assert!(!Arc::ptr_eq(&agent.client, &global_client));

        // Agent with Off: shares the global client (inherits Medium).
        let mut config = AgentConfig::default();
        config.name = "coder".to_string();
        config.reasoning_effort = crate::types::ReasoningEffort::Off;
        let agent = Agent::new(
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            None,
            global_client.clone(),
            None,
            None,
            PathBuf::new(),
        );
        assert_eq!(agent.client.reasoning_effort(), crate::types::ReasoningEffort::Medium);
        assert!(Arc::ptr_eq(&agent.client, &global_client));
    }

    #[test]
    fn test_parse_routing_object_known_agent() {
        let agent = make_agent("test");
        // Register "coder" as an invokable agent
        agent.invocation_registry.register("coder", Arc::new(MockInvokable::new("coder")));
        
        let response = r#"{"agent": "coder", "task": "write a file"}"#;
        let result = agent.parse_routing_object(response);
        assert!(result.is_some());
        let (agent_name, task) = result.unwrap();
        assert_eq!(agent_name, "coder");
        assert_eq!(task, "write a file");
    }

    #[test]
    fn test_parse_routing_object_unknown_agent() {
        let agent = make_agent("test");
        let response = r#"{"agent": "nonexistent", "task": "do something"}"#;
        assert!(agent.parse_routing_object(response).is_none());
    }

    struct MockInvokable {
        name: String,
    }
    impl MockInvokable {
        fn new(name: &str) -> Self { Self { name: name.to_string() } }
    }
    #[async_trait::async_trait]
    impl AgentInvocation for MockInvokable {
        async fn invoke(&self, _request: &str, _context: &serde_json::Value, _cancel_token: &tokio_util::sync::CancellationToken) -> Result<AgentResult, AgentError> {
            Ok(AgentResult {
                task_id: "test".to_string(),
                agent_id: self.name.clone(),
                agent_type: AgentType::General,
                status: TaskStatus::Completed,
                output: serde_json::json!("mock result"),
                summary: String::new(),
                duration_ms: 0,
                completed_at: None,
            })
        }
        fn metadata(&self) -> AgentMetadata {
            AgentMetadata {
                name: self.name.clone(),
                description: format!("Mock agent: {}", self.name),
                agent_type: AgentType::General,
                allowed_tools: vec![],
            }
        }
    }

    fn test_msg(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// An LLM that fails every call — proves verification short-circuits
    /// without any LLM round-trip when no tool outputs exist.
    struct RefuseLlm;
    #[async_trait::async_trait]
    impl LlmClient for RefuseLlm {
        async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
            Err("LLM must not be called".to_string())
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
        ) -> Result<String, String> {
            Err("LLM must not be called".to_string())
        }
    }

    /// An LLM that returns a fixed verdict and records the prompt it was
    /// given, so tests can assert what the judge actually sees.
    struct JudgeLlm {
        verdict: &'static str,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
    }
    #[async_trait::async_trait]
    impl LlmClient for JudgeLlm {
        async fn complete(&self, messages: &[Message]) -> Result<String, String> {
            self.seen.lock().unwrap().extend_from_slice(messages);
            Ok(self.verdict.to_string())
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn agent_with_llm(llm: std::sync::Arc<dyn LlmClient>) -> Agent {
        let registry = std::sync::Arc::new(ToolRegistry::new(
            vec![],
            std::sync::Arc::new(TracingToolLogger),
        ));
        Agent::new(
            AgentConfig {
                name: "test".to_string(),
                ..Default::default()
            },
            llm,
            std::sync::Arc::new(Mutex::new(ToolManager::new(registry))),
            std::sync::Arc::new(AgentInvocationRegistry::new()),
            None,
            std::sync::Arc::new(ChatClient::new("http://localhost:1")),
            None,
            None,
            PathBuf::new(),
        )
    }

    fn judge_agent(verdict: &'static str, seen: std::sync::Arc<std::sync::Mutex<Vec<Message>>>) -> Agent {
        agent_with_llm(std::sync::Arc::new(JudgeLlm { verdict, seen }))
    }

    #[tokio::test]
    async fn test_verify_no_tool_outputs_skips_llm() {
        let agent = agent_with_llm(std::sync::Arc::new(RefuseLlm));
        let messages = vec![test_msg("user", "hello"), test_msg("assistant", "hi there")];
        let result = agent
            .verify_tool_outputs(&messages, "hello", "hi there", &CancellationToken::new())
            .await;
        assert_eq!(result, Ok(true), "no tool outputs -> auto-verified without an LLM call");
    }

    #[tokio::test]
    async fn test_verify_verdict_verified() {
        let agent = judge_agent("VERIFIED", std::sync::Arc::new(Mutex::new(Vec::new())));
        let messages = vec![
            test_msg("user", "list the directory"),
            test_msg("tool", "a.txt\nb.txt"),
            test_msg("assistant", "There are two files: a.txt and b.txt."),
        ];
        assert_eq!(
            agent
                .verify_tool_outputs(&messages, "list the directory", "There are two files: a.txt and b.txt.", &CancellationToken::new())
                .await,
            Ok(true)
        );
    }

    #[tokio::test]
    async fn test_verify_verdict_needs_fix() {
        let agent = judge_agent("NEEDS_FIX: the response misses b.txt", std::sync::Arc::new(Mutex::new(Vec::new())));
        let messages = vec![
            test_msg("user", "list the directory"),
            test_msg("tool", "a.txt\nb.txt"),
            test_msg("assistant", "There is one file: a.txt."),
        ];
        assert_eq!(
            agent
                .verify_tool_outputs(&messages, "list the directory", "There is one file: a.txt.", &CancellationToken::new())
                .await,
            Ok(false)
        );
    }

    #[tokio::test]
    async fn test_verify_ambiguous_verdict_defaults_to_verified() {
        let agent = judge_agent("The answer looks plausible I guess", std::sync::Arc::new(Mutex::new(Vec::new())));
        let messages = vec![
            test_msg("user", "list the directory"),
            test_msg("tool", "a.txt"),
            test_msg("assistant", "One file."),
        ];
        assert_eq!(
            agent
                .verify_tool_outputs(&messages, "list the directory", "One file.", &CancellationToken::new())
                .await,
            Ok(true)
        );
    }

    #[tokio::test]
    async fn test_verify_prompt_includes_final_response_and_outputs() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let agent = judge_agent("VERIFIED", seen.clone());
        let messages = vec![
            test_msg("user", "read the readme"),
            test_msg("tool", "README CONTENTS HERE"),
            test_msg("assistant", "The readme says hello world."),
        ];
        agent
            .verify_tool_outputs(&messages, "read the readme", "The readme says hello world.", &CancellationToken::new())
            .await
            .unwrap();
        let joined: String = seen
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("The readme says hello world."),
            "judge prompt must include the assistant's final response: {}",
            joined
        );
        assert!(
            joined.contains("README CONTENTS HERE"),
            "judge prompt must still include the tool outputs: {}",
            joined
        );
    }
}
