use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing;

use super::config::AgentConfig;
use super::types::RunStats;
use super::LlmClient;
use crate::client::ChatClient;
use crate::memory::{MemoryEntry, MemoryManager, MemoryType};
use crate::tools::ToolManager;
use crate::trimming::ContextTrimming;
use crate::types::Message;

/// Minimum delay between LLM calls to prevent API rate-limiting (500ms).
const LLM_RATE_LIMIT_DELAY: Duration = Duration::from_millis(500);

/// Maximum verification attempts before giving up.
const MAX_VERIFICATION_ATTEMPTS: u32 = 2;

/// Verification retry nudge pushed to the request list on `NEEDS_FIX`.
///
/// Request-only: it is never written to the shared store (see `is_storable`),
/// and the verification request must be captured at loop start rather than
/// re-extracted after the nudge exists (see `run_llm_loop`).
const VERIFICATION_NUDGE: &str = "Your previous response did not fully satisfy the request. Improve it based on the tool outputs, or correct your tool calls and try again.";

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

/// S1: max characters of the judge's reason / the task snippet stored in a
/// verification-outcome lesson memory (keeps the entry compact).
const VERIFICATION_OUTCOME_REASON_CHARS: usize = 300;
const VERIFICATION_OUTCOME_TASK_CHARS: usize = 200;

/// S1: persist a verification outcome that did NOT pass on the first try.
///
/// `verdict` is `"verified_after_retry"` (the judge failed the first attempt,
/// the nudged retry passed) or `"gave_up"` (the nudge loop was exhausted).
/// Stored as a `lesson` memory (tags `agent:<name>` + `verification`, source
/// `verification`) through the shared save path — the dedup gate collapses
/// repeated identical outcomes. Returns `Ok(true)` when an entry was saved,
/// `Ok(false)` when skipped (memory disabled), `Err` on store failure.
pub fn record_verification_outcome(
    memory: &MemoryManager,
    agent_name: &str,
    verdict: &str,
    attempts: u32,
    judge_reason: &str,
    task: &str,
) -> Result<bool, String> {
    if !memory.config().enabled {
        return Ok(false);
    }
    let agent_tag = format!("agent:{agent_name}");
    let reason = if judge_reason.trim().is_empty() {
        "(no reason given)".to_string()
    } else {
        truncate_chars(judge_reason, VERIFICATION_OUTCOME_REASON_CHARS)
    };
    let entry = MemoryEntry::new(
        MemoryType::Lesson,
        &format!(
            "Verification outcome for agent '{}': {} after {} verification attempt(s). Judge: {}. Task: {}",
            agent_name,
            verdict,
            attempts,
            reason,
            truncate_chars(task, VERIFICATION_OUTCOME_TASK_CHARS)
        ),
        "verification",
        &[agent_tag.as_str(), "verification"],
    );
    memory.add(entry).map(|_| true)
}

/// Character-aware truncation for S1 outcome content.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

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

/// Maximum number of handoff hops within a single user turn. Bounds
/// handoff loops (A→B→A→…) — each hop is a full agent run, so this also
/// caps the total work a single queued turn can trigger.
const MAX_HANDOFF_DEPTH: usize = 8;

/// Outcome of one agent's LLM loop.
enum RunOutcome {
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

/// The verification judge's verdict on the assistant's final response for
/// the current turn (returned by `verify_tool_outputs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationVerdict {
    /// Whether the judge (or the no-tool-outputs shortcut) accepts the
    /// response.
    pub verified: bool,
    /// The judge's raw response text — empty for the no-tool-outputs
    /// shortcut. Kept for logging and as S1 outcome evidence.
    pub judge_reason: String,
}

/// A configurable agent that runs an LLM loop with tool calls.
///
/// Each agent has its own system prompt, allowed tools, and shell config.
#[derive(Clone)]
pub struct Agent {
    config: AgentConfig,
    llm_client: Arc<dyn LlmClient>,
    tool_manager: Arc<Mutex<ToolManager>>,
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
    /// Centralized trimming engine.
    trimming: ContextTrimming,
    /// Per-execution handoff mailbox, present only when `handoff_enabled`.
    /// The `handoff` tool writes a request here; `run_llm_loop` picks it up
    /// before the next LLM round.
    handoff_mailbox: Option<Arc<Mutex<Option<crate::agents::types::HandoffRequest>>>>,
    /// Per-execution restart mailbox, present only when `restart_enabled`.
    /// The `restart` tool writes a request here; `run_llm_loop` picks it up
    /// before the next LLM round.
    restart_mailbox: Option<Arc<Mutex<Option<crate::agents::types::RestartRequest>>>>,
    /// I1: trajectory stats of the last completed `run_llm_loop`.
    run_stats: RunStats,
    /// Mid-run injection channel (UI → this run), if the chat pipeline
    /// attached one. The agent loop drains it at LLM round boundaries: a user
    /// message sent while the run is active is appended to the current turn
    /// and seen by the model on the very next LLM call. Moved to the next
    /// agent on a `handoff` so the whole chain keeps receiving injections.
    injection_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<crate::sessions::QueuedMessage>>>>,
}

impl Agent {
    /// Create a new agent from config.
    pub fn new(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<Mutex<ToolManager>>,
        event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
        client: Arc<ChatClient>,
        memory: Option<Arc<crate::memory::MemoryManager>>,
        agent_session_id: Option<String>,
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
        // Token tracker: stamp this agent's name on the usage-log lines this
        // client writes. Agents within a session run sequentially, so the
        // shared client's name is always current at request time.
        client.set_agent_name(&config.name);
        // Give this agent its own tool manager whose `shell` honors the agent's
        // shell config (allowlist/enabled/timeout), instead of sharing the global
        // allow-all shell. All other tools are shared. This is what makes an
        // agent's `shell` respect its per-agent restrictions on both the chat and
        // /plan paths.
        let (tool_manager, handoff_mailbox, restart_mailbox) = {
            let shared = tool_manager.lock().unwrap();
            // The shell tool is advertised only when the agent's
            // `shell_enabled` is true. A disabled shell is removed from the
            // schema entirely instead of remaining as a tool whose calls
            // always error out.
            let tm = if config.get_shell_config().shell_enabled {
                shared.with_shell_config(config.get_shell_config())
            } else {
                shared.without_shell()
            };
            // The handoff tool is advertised only when `handoff_enabled` —
            // gated by flag, like the shell above, NOT by `allowed_tools`.
            // Each execution gets its own mailbox + tool instance (per-agent
            // target allowlist, agents dir), injected exactly like the shell.
            let (tm, handoff_mailbox) = if config.handoff_enabled {
                let mailbox = Arc::new(Mutex::new(None));
                let tool = crate::tools::builtin::handoff::HandoffTool::new(
                    mailbox.clone(),
                    config.agents_dir.clone(),
                    config.agents_search_dirs.clone(),
                    config.handoff_targets.clone(),
                );
                (tm.with_handoff_tool(tool), Some(mailbox))
            } else {
                // Drop any handoff tool inherited from a previous agent in a
                // handoff chain (the shared base manager may carry one).
                (tm.without_handoff(), None)
            };
            // The restart tool is advertised only when `restart_enabled` —
            // same flag-gated, per-execution injection as handoff/shell.
            let (tm, restart_mailbox) = if config.restart_enabled {
                let mailbox = Arc::new(Mutex::new(None));
                let tool = crate::tools::builtin::restart::RestartTool::new(mailbox.clone());
                (tm.with_restart_tool(tool), Some(mailbox))
            } else {
                // Drop any restart tool inherited from a previous agent in a
                // handoff chain (the shared base manager may carry one).
                (tm.without_restart(), None)
            };
            (Arc::new(Mutex::new(tm)), handoff_mailbox, restart_mailbox)
        };
        Self {
            config,
            llm_client,
            tool_manager,
            event_tx,
            last_llm_call_at: Instant::now(),
            client,
            memory,
            messages: Vec::new(),
            agent_session_id,
            trimming: ContextTrimming::new(),
            handoff_mailbox,
            restart_mailbox,
            run_stats: RunStats::default(),
            injection_rx: None,
        }
    }

    /// Attach the current run's mid-run injection channel (see the
    /// `injection_rx` field). Only the chat path sets this; plan/registry
    /// agents run without a live UI and never receive injections.
    pub fn with_injection_channel(
        mut self,
        rx: Arc<Mutex<std::sync::mpsc::Receiver<crate::sessions::QueuedMessage>>>,
    ) -> Self {
        self.injection_rx = Some(rx);
        self
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
    pub fn build_initial_messages(&self, task: &str) -> Vec<Message> {
        let now = crate::types::format_timestamp();
        let mut messages: Vec<Message> = Vec::new();

        // Start with the fresh system prompt (request-only, never stored).
        // The task is passed so memory injection can be query-aware.
        messages.push(Message {
            role: "system".to_string(),
            content: self.build_system_prompt(task),
            timestamp: now,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
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
        client: Arc<ChatClient>,
        memory: Option<Arc<crate::memory::MemoryManager>>,
    ) -> Self {
        let tool_registry = Arc::new(crate::tools::registry::ToolRegistry::new(
            vec![],
            Arc::new(crate::tools::types::TracingToolLogger),
        ));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        Self::new(config, llm_client, tool_manager, None, client, memory, None)
    }

    fn send_event(&self, event: crate::types::AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }

    /// Build a progress sink for a running tool call. Reports arrive as
    /// `ToolCallProgress` events (latest-tail semantics) which the UI renders
    /// in the live tool card. Returns a no-op sink when no UI channel is
    /// attached (tests, headless runs).
    fn tool_progress_for(
        &self,
        tool_name: &str,
        call_id: &str,
    ) -> crate::tools::types::ToolProgress {
        match &self.event_tx {
            Some(tx) => {
                let tx = std::sync::Arc::clone(tx);
                let name = tool_name.to_string();
                let id = call_id.to_string();
                let sid = self.session_id();
                crate::tools::types::ToolProgress {
                    on_progress: Some(std::sync::Arc::new(move |text: &str| {
                        if let Ok(g) = tx.lock() {
                            let _ = g.send(crate::types::AppEvent::ToolCallProgress {
                                tool_name: name.clone(),
                                call_id: id.clone(),
                                text: text.to_string(),
                                session_id: sid.clone(),
                            });
                        }
                    })),
                }
            }
            None => crate::tools::types::ToolProgress::none(),
        }
    }

    /// The session ID to stamp on events (falls back to empty when unset).
    fn session_id(&self) -> String {
        self.agent_session_id.clone().unwrap_or_default()
    }

    /// Take a pending handoff request written by the `handoff` tool (if any).
    fn take_pending_handoff(&self) -> Option<crate::agents::types::HandoffRequest> {
        self.handoff_mailbox
            .as_ref()
            .and_then(|m| m.lock().unwrap().take())
    }

    /// Take a pending restart request written by the `restart` tool (if any).
    fn take_pending_restart(&self) -> Option<crate::agents::types::RestartRequest> {
        self.restart_mailbox
            .as_ref()
            .and_then(|m| m.lock().unwrap().take())
    }

    /// Get the messages from the last execution for memory extraction.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// I1: trajectory stats (tool calls, tool errors, verification attempts)
    /// of the last completed `run_llm_loop` — fed to the improver.
    pub fn run_stats(&self) -> RunStats {
        self.run_stats
    }

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

    /// Build the system prompt for this agent.
    /// `query` is the user's current request; it makes memory injection query-aware.
    fn build_system_prompt(&self, query: &str) -> String {
        let mut prompt = if self.config.system_prompt.is_empty() {
            format!(
                "You are the '{}' agent. {}",
                self.config.name, self.config.description
            )
        } else {
            self.config.system_prompt.clone()
        };

        // Add memory management tool guidance
        prompt.push_str(
            "\n\nYou have memory management tools (save_memory, update_memory, search_memory, consolidate_memories, delete_memory). Use them proactively:\n",
        );
        prompt.push_str(
            "- search_memory: Search memory before starting tasks and before saving anything new\n",
        );
        prompt.push_str("- save_memory: Save non-obvious facts, lessons, or decisions as you discover them during work\n");
        prompt.push_str("- update_memory: Refine an existing entry (by ID) instead of re-adding similar information\n");
        prompt.push_str(
            "- consolidate_memories: Merge related entries (by IDs) into one comprehensive entry\n",
        );
        prompt.push_str("- delete_memory: Remove entries that turn out to be stale or wrong\n");
        prompt.push_str(&format!(
            "Tool responses include entry IDs - use them for updates, consolidation, and deletion. \
             Only save information that is persistent and useful across sessions; don't save routine \
             operations or temporary information. When saving a lesson, tag it with your agent name \
             (tags: [\"agent:{}\", ...]) so per-agent improvement checks can find it.",
            self.config.name
        ));

        // Handoff guidance: only when the agent actually has the tool.
        if self.config.handoff_enabled {
            prompt.push_str(
                "\n\n## HANDOFF\n\
                 You can switch the session to a different agent by calling the `handoff` tool with:\n\
                 - `agent`: the target agent's profile name (e.g. \"coder\")\n\
                 - `task`: what the target agent should do next (include the context it needs — it sees the full conversation too)\n\
                 Call it when your part of the work is complete and another specialist should continue \
                 (e.g. after finishing a plan, hand off to a coder to implement it). \
                 Your turn ends when you call it; the session continues with the target agent.",
            );
            if !self.config.handoff_targets.is_empty() {
                prompt.push_str(&format!(
                    "\nYou may only hand off to: {}.",
                    self.config.handoff_targets.join(", ")
                ));
            }
        }
        // Restart guidance: only when the agent actually has the tool. Emphasize
        // the Windows `--target-dir` self-build pattern (dogfooding WuffAgent's
        // own source: edit → build → restart to load the new code → resume).
        if self.config.restart_enabled {
            prompt.push_str(
                "\n\n## RESTART\\\n\
                 You can restart WuffAgent — then resume this session automatically — by calling the `restart` tool with:\\n\\\n\
                 - `reason` (required): what you changed and why you are restarting; shown to the user and used to resume the work\\n\\\n\
                 - `build_cmd` (optional): a command to run FIRST (e.g. a rebuild); if it fails the restart is skipped so you can fix it\\n\\\n\
                 - `exe_path` (optional): the binary to launch; omit to relaunch the current executable\\n\\\n\
                 Use it after making changes that require a rebuild. Most useful when editing WuffAgent's own source. \
                 For WuffAgent itself, OMIT build_cmd and exe_path: the tool then builds and launches the OTHER of \
                 WuffAgent's two standard builds — the default `cargo build` output (target/debug) and a second copy \
                 (target/relaunch) — alternating between them on every restart, since on Windows the running exe \
                 cannot be relinked in place. Your turn ends when you call it; WuffAgent closes and reopens, then \
                 continues the same work.",
            );
        }

        // Inject memories relevant to the current request (query-aware)
        if let Some(memory) = &self.memory {
            let memory_block = memory.build_context_block(query);
            if !memory_block.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&memory_block);
            }
        }

        // Note: system prompt caching would require &'mut self, which conflicts
        // with the LLM loop. The prompt is cheap to rebuild (~100ns).
        prompt
    }

    /// Whether a message belongs in the shared store (vs. request-only).
    ///
    /// System messages (the per-run system prompt) and the verification retry
    /// nudge belong only in outgoing requests and are never persisted. Empty
    /// assistant placeholders (no content, no tool calls) are a streaming
    /// artifact and are never stored either.
    ///
    /// The nudge is recognized by its content AND its empty timestamp: the
    /// loop pushes it with `timestamp: String::new()`, while real user
    /// messages always carry `format_timestamp()`. Matching content alone
    /// would drop a user message that happens to repeat the nudge verbatim.
    fn is_storable(msg: &Message) -> bool {
        if msg.role == "system" {
            return false;
        }
        if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
            return false;
        }
        !(msg.role == "user" && msg.content == VERIFICATION_NUDGE && msg.timestamp.is_empty())
    }

    /// Reconcile the shared store with the per-run request list after a trim.
    ///
    /// The store is the single source of truth, but on the agent path only
    /// the throwaway request list is trimmed — without this, the store (and
    /// the session file) grows without bound. The request list is the store's
    /// storable projection plus request-only entries (system prompt, nudge),
    /// so replacing the store with that projection drops exactly the messages
    /// the trim removed and keeps store and request list in sync for every
    /// later turn (including after a session reload).
    fn reconcile_store(&self, messages: &[Message]) {
        let projected: Vec<Message> = messages
            .iter()
            .filter(|m| Self::is_storable(m))
            .cloned()
            .collect();
        let conv = self.client.conversation();
        *conv.lock().unwrap() = projected;
    }

    /// Record a generated message in the shared store, exactly once.
    ///
    /// Request-only messages (system prompt, verification nudge — see
    /// `is_storable`) are skipped. This mirrors the client's non-agent path,
    /// where the user and assistant messages are written to the shared
    /// conversation as they happen and the system prompt is never stored.
    fn record_in_store(&self, msg: &Message) {
        if !Self::is_storable(msg) {
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
                        .and_then(crate::client::image_source_data_uri);
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
                let total_chars = crate::trimming::message_char_count(messages);
                if msg_count > 4 && total_chars > self.client.trim_trigger_chars() {
                    // Target in char units: 50% of n_ctx tokens converted to
                    // chars via the client's calibrated chars-per-token ratio.
                    let target_chars = self.client.trim_target_chars();
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
            let pending_tool_runs: Arc<
                Mutex<
                    std::collections::HashMap<
                        String,
                        tokio::task::JoinHandle<Result<String, String>>,
                    >,
                >,
            > = Arc::new(Mutex::new(std::collections::HashMap::new()));

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
                    let ready_tx = self.event_tx.clone();
                    let ready_sid = self.session_id();
                    let pp_tx = self.event_tx.clone();
                    let pp_sid = self.session_id();
                    let ready_pending = Arc::clone(&pending_tool_runs);
                    let ready_manager = tool_manager.clone();
                    let ready_cancel = cancel_token.clone();
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
                        move |call: crate::types::ToolCall| {
                            // Early-start this tool call while the model is
                            // still streaming: the SSE layer only reports a
                            // call once its arguments are complete, so it is
                            // safe to execute now.
                            if call.id.is_empty() {
                                return;
                            }
                            let id = call.id.clone();
                            if ready_pending.lock().unwrap().contains_key(&id) {
                                return; // defensive: already started
                            }
                            let name = call.function.name.clone();
                            let args = call.function.arguments.clone();
                            tracing::debug!(
                                "[AGENT] Early-starting tool '{}' (id={}) while model is still streaming",
                                name, id
                            );
                            // Live tool card: args preview + progress sink.
                            let args_preview = crate::tools::tool_args_summary(&name, &args);
                            if let Some(ref tx) = ready_tx {
                                if let Ok(g) = tx.lock() {
                                    let _ = g.send(crate::types::AppEvent::ToolCallStart {
                                        tool_name: name.clone(),
                                        call_id: id.clone(),
                                        args_preview,
                                        session_id: ready_sid.clone(),
                                    });
                                }
                            }
                            let progress = match &ready_tx {
                                Some(tx) => {
                                    let tx = std::sync::Arc::clone(tx);
                                    let p_name = name.clone();
                                    let p_id = id.clone();
                                    let p_sid = ready_sid.clone();
                                    crate::tools::types::ToolProgress {
                                        on_progress: Some(std::sync::Arc::new(move |text: &str| {
                                            if let Ok(g) = tx.lock() {
                                                let _ = g.send(crate::types::AppEvent::ToolCallProgress {
                                                    tool_name: p_name.clone(),
                                                    call_id: p_id.clone(),
                                                    text: text.to_string(),
                                                    session_id: p_sid.clone(),
                                                });
                                            }
                                        })),
                                    }
                                }
                                None => crate::tools::types::ToolProgress::none(),
                            };
                            let tool_mgr = ready_manager.clone();
                            let token = ready_cancel.clone();
                            let handle = tokio::spawn(async move {
                                let params = match crate::tools::manager::parse_tool_args(&args) {
                                    Ok(p) => p,
                                    Err(e) => return Err(e),
                                };
                                let result = tokio::select! {
                                    r = tool_mgr.execute_with_progress(&name, params, &progress) => r,
                                    _ = token.cancelled() => {
                                        return Ok(format!(
                                            "Error: Cancelled (tool '{}' aborted)",
                                            name
                                        ))
                                    }
                                };
                                Ok(match result {
                                    Ok(output) => {
                                        let s = format!("{}", output);
                                        if s.trim().is_empty() {
                                            "(no output)".to_string()
                                        } else {
                                            s
                                        }
                                    }
                                    Err(e) => format!("Error: {}", e),
                                })
                            });
                            ready_pending.lock().unwrap().insert(id, handle);
                        },
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
                            for h in pending_tool_runs.lock().unwrap().values() {
                                h.abort();
                            }
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
                            let target = self.client.overflow_retry_char_budget(&ov);
                            let removed = self
                                .trimming
                                .trim_messages(messages, target, &self.config.trim_config);
                            self.reconcile_store(messages);
                            tracing::info!(
                                "[AGENT] Agent '{}' force-trim removed {} messages (target_chars={})",
                                self.config.name, removed, target
                            );
                            self.client.note_prompt_chars(crate::trimming::message_char_count(messages));
                            for h in pending_tool_runs.lock().unwrap().values() {
                                h.abort();
                            }
                            pending_tool_runs.lock().unwrap().clear();
                            continue;
                        }
                        Err(e) => {
                            for h in pending_tool_runs.lock().unwrap().values() {
                                h.abort();
                            }
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
            let assistant_msg_rec = Message {
                role: "assistant".to_string(),
                content: content.clone(),
                timestamp: crate::types::format_timestamp(),
                tool_calls: tool_calls.clone(),
                tool_call_id: None,
                reasoning_content: reasoning.clone(),
                image: None,
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
            // Calls the model has already moved past were early-started
            // mid-stream (ready callback above) — collect their results
            // here, in call order. Calls that never got a "moved past"
            // signal (typically the LAST one in the stream) are executed
            // inline, exactly as before.
            if let Some(calls) = &tool_calls {
                if !calls.is_empty() {
                    for call in calls {
                        if cancel_token.is_cancelled() {
                            // Stop early-started runs that have not been
                            // collected yet so nothing keeps executing in
                            // the background for a dead turn.
                            for h in pending_tool_runs.lock().unwrap().values() {
                                h.abort();
                            }
                            return Err("Cancelled".to_string());
                        }
                        // Bind the handle out of the map before awaiting: the
                        // mutex guard must not live across the await (the
                        // enclosing future must stay Send).
                        let early_handle = pending_tool_runs.lock().unwrap().remove(&call.id);
                        let result_str: String = if let Some(handle) = early_handle {
                            match handle.await {
                                Ok(Ok(s)) => s,
                                Ok(Err(bad_args)) => {
                                    tracing::warn!(
                                        "[AGENT] Bad args for '{}': {}",
                                        call.function.name,
                                        bad_args
                                    );
                                    self.send_event(crate::types::AppEvent::ToolCallError {
                                        tool_name: call.function.name.clone(),
                                        call_id: call.id.clone(),
                                        error: bad_args.clone(),
                                        session_id: self.session_id(),
                                    });
                                    format!("Error: {}", bad_args)
                                }
                                Err(join_err) => {
                                    let e = format!("tool task failed: {}", join_err);
                                    self.send_event(crate::types::AppEvent::ToolCallError {
                                        tool_name: call.function.name.clone(),
                                        call_id: call.id.clone(),
                                        error: e.clone(),
                                        session_id: self.session_id(),
                                    });
                                    format!("Error: {}", e)
                                }
                            }
                        } else {
                            // Inline fallback: the stream ended while this
                            // call was still the active one.
                            self.send_event(crate::types::AppEvent::ToolCallStart {
                                tool_name: call.function.name.clone(),
                                call_id: call.id.clone(),
                                args_preview: crate::tools::tool_args_summary(
                                    &call.function.name,
                                    &call.function.arguments,
                                ),
                                session_id: self.session_id(),
                            });
                            let params = match crate::tools::manager::parse_tool_args(
                                &call.function.arguments,
                            ) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!(
                                        "[AGENT] Bad args for '{}': {}",
                                        call.function.name,
                                        e
                                    );
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
                                        image: None,
                                    };
                                    messages.push(bad_args_msg.clone());
                                    self.record_in_store(&bad_args_msg);
                                    continue;
                                }
                            };
                            let manager = tool_manager.clone();
                            let progress = self.tool_progress_for(&call.function.name, &call.id);
                            let tool_result = manager
                                .execute_with_progress(&call.function.name, params, &progress)
                                .await;
                            // Tools must always return *something*: an empty result
                            // string becomes an empty `role: "tool"` message, which
                            // the model/server rejects.
                            match tool_result {
                                Ok(output) => {
                                    let s = format!("{}", output);
                                    if s.trim().is_empty() {
                                        "(no output)".to_string()
                                    } else {
                                        s
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "[AGENT] Tool '{}' failed: {}",
                                        call.function.name,
                                        e
                                    );
                                    format!("Error: {}", e)
                                }
                            }
                        };
                        // Early-started calls sent their ToolCallStart mid-stream;
                        // emit the completion in call order now.
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
                            image: None,
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
                            args_preview: crate::tools::tool_args_summary(
                                &call.function.name,
                                &call.function.arguments,
                            ),
                            session_id: self.session_id(),
                        });
                        let params = match crate::tools::manager::parse_tool_args(
                            &call.function.arguments,
                        ) {
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
                        let manager = tool_manager.clone();
                        let progress = self.tool_progress_for(&call.function.name, &call.id);
                        let tool_result = manager
                            .execute_with_progress(&call.function.name, params, &progress)
                            .await;
                        let result_str = match tool_result {
                            Ok(output) => {
                                let s = format!("{}", output);
                                if s.trim().is_empty() {
                                    "(no output)".to_string()
                                } else {
                                    s
                                }
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
                            image: None,
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
                            image: None,
                        };
                        messages.push(fb_tool.clone());
                        self.record_in_store(&fb_tool);
                    }
                    continue;
                }
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

    /// I1: count tool calls and errors in `messages[from..]` (this run's
    /// portion only — earlier turns of a multi-turn conversation are excluded)
    /// and combine with the verification attempts into `RunStats`.
    ///
    /// Tool errors are detected by the "Error: " prefix the loop writes into
    /// failed tool results; a successful tool output that merely STARTS with
    /// that text (rare) is over-counted — acceptable for advisory evidence.
    fn run_stats_since(messages: &[Message], from: usize, verification_attempts: u32) -> RunStats {
        let mut tool_calls = 0usize;
        let mut tool_errors = 0usize;
        for m in messages.iter().skip(from) {
            if let Some(calls) = &m.tool_calls {
                tool_calls += calls.len();
            }
            if m.role == "tool" && m.content.starts_with("Error: ") {
                tool_errors += 1;
            }
        }
        RunStats {
            tool_calls,
            tool_errors,
            verification_attempts,
        }
    }

    /// Extract the current turn's user request from the message history.
    ///
    /// Uses the LAST user message. Call it ONCE at the start of the run,
    /// before any verification nudge is pushed: after a NEEDS_FIX the last
    /// user message is the nudge, and re-extracting would make the judge
    /// grade the response against the nudge instead of the real request.
    /// (In a multi-turn session the first user message is stale for the same
    /// reason — the last one is the request being answered.)
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
    ) -> Result<VerificationVerdict, String> {
        // Scope the evidence to the current turn: only tool outputs produced
        // after the most recent user message count as evidence for this
        // turn's response. In a multi-turn session the full history holds stale
        // tool results from earlier turns; feeding those to the judge made it
        // return NEEDS_FIX for a perfectly complete answer to the current
        // request, which then re-asked the model after it had already finished.
        let turn_start = messages.iter().rposition(|m| m.role == "user").unwrap_or(0);
        let tool_outputs: Vec<String> = messages
            .iter()
            .skip(turn_start)
            .filter(|m| m.role == "tool")
            // Truncate on the &str: the full tool output (potentially 100KB+)
            // is never needed — only the summary prefix is.
            .map(|m| m.content.chars().take(TOOL_OUTPUT_SUMMARY_CHARS).collect())
            .collect();

        // Skip verification if no tool calls were made this turn.
        if tool_outputs.is_empty() {
            return Ok(VerificationVerdict {
                verified: true,
                judge_reason: String::new(),
            });
        }
        let recent_tool_summary: String = tool_outputs.join("\n");

        let verification_messages = vec![
            Message {
                role: "system".to_string(),
                content: VERIFICATION_SYSTEM_PROMPT.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                image: None,
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
                image: None,
            },
        ];

        // Check cancellation before making the LLM call.
        if cancel_token.is_cancelled() {
            return Err("Verification cancelled".to_string());
        }

        // The judge call goes through the STREAMING path (like the main LLM
        // rounds), not the non-streaming one. The non-streaming client's
        // TOTAL request timeout (ChatClient::DEFAULT_TIMEOUT_SECS = 300 s)
        // used to fire here: on a slow local model a judge response can
        // legitimately take minutes to generate (e.g. a long thinking
        // block), so every session end stalled for 5 minutes and logged
        // "Verification LLM call failed: HTTP error: error sending request".
        // The streaming client has no total timeout — only a 300 s idle
        // read timeout that still bounds a truly hung server (no bytes
        // flowing for 5 min). An earlier fixed 60 s-per-attempt cap was
        // removed for the same slow-local-model reason.
        let judge_started = Instant::now();
        let cancel_clone = cancel_token.clone();
        let result = tokio::select! {
            result = self.llm_client.stream(&verification_messages, Box::new(|_chunk: String| {})) => result,
            _ = cancel_clone.cancelled() => Err("Verification cancelled".to_string()),
        };
        tracing::debug!(
            "[AGENT] Verification judge call finished in {:.1}s (ok={})",
            judge_started.elapsed().as_secs_f32(),
            result.is_ok()
        );
        let response = match result {
            Ok(r) => r,
            Err(e) => return Err(format!("Verification LLM call failed: {}", e)),
        };

        // Robust verification: check NEEDS_FIX first (takes precedence),
        // then check if the response is clearly affirmative.
        let response_upper = response.to_uppercase();
        if response_upper.contains("NEEDS_FIX")
            || response_upper.contains("NOT SATISFIED")
            || response_upper.contains("INCORRECT")
            || response_upper.contains("INCOMPLETE")
        {
            Ok(VerificationVerdict {
                verified: false,
                judge_reason: response,
            })
        } else {
            // VERIFIED, or unclear — default to verified (better to continue
            // than to abort a successful execution on an ambiguous LLM
            // response). The judge text is kept either way for S1 evidence.
            Ok(VerificationVerdict {
                verified: true,
                judge_reason: response,
            })
        }
    }

    /// S1: store a non-first-try verification outcome as a lesson memory.
    /// No-op when the agent has no memory manager; store failures are logged,
    /// never fatal to the run.
    fn store_verification_outcome(
        &self,
        verdict: &str,
        attempts: u32,
        judge_reason: &str,
        task: &str,
    ) {
        let Some(memory) = &self.memory else {
            return;
        };
        match record_verification_outcome(
            memory,
            &self.config.name,
            verdict,
            attempts,
            judge_reason,
            task,
        ) {
            Ok(true) => tracing::debug!(
                "[AGENT] Stored verification outcome for '{}' ({})",
                self.config.name,
                verdict
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!(
                "[AGENT] Failed to store verification outcome for '{}': {}",
                self.config.name,
                e
            ),
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
                                    id: v
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string(),
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
                    if c == '[' {
                        bracket_depth += 1;
                    }
                    if c == '{' {
                        brace_depth += 1;
                    }
                }
                c if !in_string && (c == ']' || c == '}') => {
                    if c == ']' {
                        bracket_depth -= 1;
                    }
                    if c == '}' {
                        brace_depth -= 1;
                    }
                    // Only close a region if we're closing the matching depth-0 opener.
                    if bracket_depth < 0 {
                        bracket_depth = 0;
                    }
                    if brace_depth < 0 {
                        brace_depth = 0;
                    }
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

    /// Extract bash/code blocks from LLM responses and convert them to the
    /// named file tools (read_file, list_dir, search_files, search_content,
    /// mkdir, delete, copy, move, ...). Only commands without a named
    /// equivalent (pwd) fall through to the generic shell tool.
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
                let cmd_line = if lines.len() > 1
                    && (lines[0] == "bash" || lines[0] == "sh" || lines[0] == "shell")
                {
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
                        let pattern = format!("{path}/*");
                        emit_tool("search_files", serde_json::json!({ "pattern": pattern }));
                    }
                    "grep" => {
                        // grep [-flags] pattern [path]
                        if let Some(pattern) = args_parts.first() {
                            let path = args_parts.get(1).copied().unwrap_or(".");
                            let is_regex = raw_parts.iter().any(|p| *p == "-E" || *p == "-P");
                            let case_sensitive = !raw_parts.iter().any(|p| *p == "-i");
                            emit_tool(
                                "search_content",
                                serde_json::json!({
                                    "pattern": pattern,
                                    "path": path,
                                    "regex": is_regex,
                                    "case_sensitive": case_sensitive,
                                }),
                            );
                        }
                    }
                    "head" | "tail" => {
                        if let Some(path) = args_parts.last() {
                            emit_tool("read_file", serde_json::json!({ "path": path }));
                        }
                    }
                    "mkdir" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        let recursive = raw_parts.iter().any(|p| *p == "-p" || *p == "--parents");
                        emit_tool(
                            "mkdir",
                            serde_json::json!({ "path": path, "recursive": recursive }),
                        );
                    }
                    "rm" => {
                        for path in &args_parts {
                            let recursive = raw_parts.iter().any(|p| {
                                *p == "-r"
                                    || *p == "-R"
                                    || *p == "-rf"
                                    || *p == "-fr"
                                    || *p == "--recursive"
                            });
                            emit_tool(
                                "delete",
                                serde_json::json!({ "path": path, "recursive": recursive }),
                            );
                        }
                    }
                    "cp" | "mv" => {
                        if args_parts.len() >= 2 {
                            let name = if cmd == "cp" { "copy" } else { "move" };
                            emit_tool(
                                name,
                                serde_json::json!({
                                    "src": args_parts[0],
                                    "dest": args_parts[args_parts.len() - 1],
                                }),
                            );
                        }
                    }
                    // No named file tool equivalent — run via the shell tool.
                    "pwd" => {
                        emit_tool("shell", serde_json::json!({ "command": cmd_line }));
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
mod tests;
