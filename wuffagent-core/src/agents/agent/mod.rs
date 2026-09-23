use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::config::AgentConfig;
use super::types::RunStats;
use super::LlmClient;
use crate::client::ChatClient;
use crate::tools::ToolManager;
use crate::trimming::ContextTrimming;
use crate::types::Message;

pub(crate) mod memory_sync;
pub(crate) mod prompt;
pub(crate) mod r#loop;
pub(crate) mod stats;
pub(crate) mod tool_exec;
pub(crate) mod toolcall_parse;
pub(crate) mod verify;

/// Verification retry nudge pushed to the request list on `NEEDS_FIX`.
///
/// Request-only: it is never written to the shared store (see `is_storable`),
/// and the verification request must be captured at loop start rather than
/// re-extracted after the nudge exists (see `run_llm_loop`).
pub(crate) const VERIFICATION_NUDGE: &str = "Your previous response did not fully satisfy the request. Improve it based on the tool outputs, or correct your tool calls and try again.";

/// Minimum number of messages required before trimming is attempted.
#[allow(dead_code)]
const MIN_MESSAGES_FOR_TRIM: usize = 4;

/// Character-aware truncation for S1 outcome content.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
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

    pub(crate) fn send_event(&self, event: crate::types::AppEvent) {
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
    pub(crate) fn tool_progress_for(
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
    pub(crate) fn session_id(&self) -> String {
        self.agent_session_id.clone().unwrap_or_default()
    }

    /// Take a pending handoff request written by the `handoff` tool (if any).
    pub(crate) fn take_pending_handoff(&self) -> Option<crate::agents::types::HandoffRequest> {
        self.handoff_mailbox
            .as_ref()
            .and_then(|m| m.lock().unwrap().take())
    }

    /// Take a pending restart request written by the `restart` tool (if any).
    pub(crate) fn take_pending_restart(&self) -> Option<crate::agents::types::RestartRequest> {
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

}


#[cfg(test)]
mod tests;
