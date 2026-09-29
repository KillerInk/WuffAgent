use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::config::AgentConfig;
use super::types::RunStats;
use super::LlmClient;
use crate::client::ChatClient;
use crate::tools::ToolManager;
use crate::trimming::ContextTrimming;
use crate::types::Message;

pub(crate) mod execute;
pub(crate) mod inject;
pub(crate) mod memory_sync;
pub(crate) mod prompt;
pub(crate) mod r#loop;
pub(crate) mod stats;
pub(crate) mod tool_calls;
pub(crate) mod tool_exec;
pub(crate) mod toolcall_parse;
pub(crate) mod verify;

/// Verification retry nudge pushed to the request list on `NEEDS_FIX`.
///
/// Request-only: it is never written to the shared store (see `is_storable`),
/// and the verification request must be captured at loop start rather than
/// re-extracted after the nudge exists (see `run_llm_loop`).
pub(crate) const VERIFICATION_NUDGE: &str = "Your previous response did not fully satisfy the request. Improve it based on the tool outputs, or correct your tool calls and try again.";

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
    tool_manager: Arc<ToolManager>,
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
    /// S2: the single per-execution control mailbox shared by the `handoff`,
    /// `restart`, `hand_back` and `session_note` tools. Each enabled tool
    /// pushes its own `ControlRequest` variant; `run_llm_loop` drains it once
    /// per LLM round boundary (notes are applied and the run continues, a
    /// pending handoff/restart/hand-back ends the run). Always present — when
    /// none of the tools is injected, draining it is a no-op.
    control_mailbox: Arc<Mutex<Vec<crate::agents::types::ControlRequest>>>,
    /// I1: trajectory stats of the last completed `run_llm_loop`.
    run_stats: RunStats,
    /// 1b: the app config's model price table (cost_usd estimates on run
    /// lines; empty = all runs "recorded but unpriced").
    model_prices: Vec<crate::config::ModelPrice>,
    /// 2b: prompt tokens consumed by the last completed `run_llm_loop` (kept
    /// separate from `RunStats` so the existing `RunStats` literals stay
    /// untouched; the eval harness reads this for its cost record).
    run_tokens_in: u64,
    /// 2b: completion tokens produced by the last completed `run_llm_loop`.
    run_tokens_out: u64,
    /// Mid-run injection channel (UI → this run), if the chat pipeline
    /// attached one. The agent loop drains it at LLM round boundaries: a user
    /// message sent while the run is active is appended to the current turn
    /// and seen by the model on the very next LLM call. Moved to the next
    /// agent on a `handoff` so the whole chain keeps receiving injections.
    injection_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<crate::sessions::QueuedMessage>>>>,
}

/// Builder for [`Agent`] (see [`Agent::builder`]).
///
/// Required: `config`, `llm_client`, `client`. Optional: `tool_manager`
/// (defaults to an empty registry — headless paths register their tools into
/// a shared manager beforehand), `event_tx`, `memory`, `agent_session_id`.
///
/// The per-agent policy (reasoning effort on a client clone, shell gating,
/// and the handoff/restart/hand_back/session_note tool injection wired to
/// the shared control mailbox) lives in exactly one place:
/// [`AgentBuilder::build`].
#[derive(Clone)]
pub struct AgentBuilder {
    config: AgentConfig,
    llm_client: Arc<dyn LlmClient>,
    client: Arc<ChatClient>,
    tool_manager: Option<Arc<ToolManager>>,
    event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
    memory: Option<Arc<crate::memory::MemoryManager>>,
    agent_session_id: Option<String>,
    model_prices: Vec<crate::config::ModelPrice>,
}

impl AgentBuilder {
    /// Start a builder with the three required inputs.
    pub fn new(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        client: Arc<ChatClient>,
    ) -> Self {
        Self {
            config,
            llm_client,
            client,
            tool_manager: None,
            event_tx: None,
            memory: None,
            agent_session_id: None,
            model_prices: Vec::new(),
        }
    }

    /// Use this tool manager (defaults to an empty registry).
    pub fn tool_manager(mut self, tool_manager: Arc<ToolManager>) -> Self {
        self.tool_manager = Some(tool_manager);
        self
    }

    /// UI event channel for agent events (None = no UI, e.g. tests).
    pub fn event_tx(
        mut self,
        event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
    ) -> Self {
        self.event_tx = event_tx;
        self
    }

    /// Memory manager for persistent context (None = memory disabled).
    pub fn memory(mut self, memory: Option<Arc<crate::memory::MemoryManager>>) -> Self {
        self.memory = memory;
        self
    }

    /// Session ID for this agent's persistent conversation (None = none).
    pub fn agent_session_id(mut self, agent_session_id: Option<String>) -> Self {
        self.agent_session_id = agent_session_id;
        self
    }

    /// 1b: the app config's model price table (cost_usd estimates on run
    /// lines; empty = all runs "recorded but unpriced").
    pub fn model_prices(mut self, model_prices: Vec<crate::config::ModelPrice>) -> Self {
        self.model_prices = model_prices;
        self
    }

    /// Build the agent.
    ///
    /// Applies the per-agent policy:
    /// - the agent's reasoning effort goes on its own client clone (Off =
    ///   inherit the global client setting);
    /// - the agent's name is stamped on the usage-log lines its client
    ///   writes (agents within a session run sequentially, so the shared
    ///   client's name is always current at request time);
    /// - the `shell` tool is advertised only when the agent's `shell_enabled`
    ///   is true (a disabled shell is removed from the schema entirely, with
    ///   the agent's shell config — allowlist/timeout — applied when enabled);
    /// - the `handoff`/`restart`/`hand_back` tools are injected exactly when
    ///   their config flag is set (hand_back additionally requires a
    ///   sub-session, i.e. a `parent_session_id` in the client's session
    ///   meta); tools inherited from a previous agent in a handoff chain are
    ///   dropped when the flag is off;
    /// - the `session_note` tool (S4a) is injected when `session_note_enabled`
    ///   (default true): `run_llm_loop` turns a queued note into an anchored
    ///   user message (right after the system prompt) + a store record, so
    ///   pinned state survives trims and session reloads.
    ///
    /// All four tools share ONE per-execution control mailbox
    /// (`Agent::control_mailbox`); `build` creates it up front and wires it
    /// into whichever tools are enabled.
    pub fn build(self) -> Agent {
        let AgentBuilder {
            config,
            llm_client,
            client,
            tool_manager,
            event_tx,
            memory,
            agent_session_id,
            model_prices,
        } = self;
        let client = if config.reasoning_effort != crate::types::ReasoningEffort::Off {
            let c = (*client).clone();
            c.set_reasoning_effort(config.reasoning_effort);
            Arc::new(c)
        } else {
            client
        };
        // Token tracker: stamp this agent's name on the usage-log lines this
        // client writes.
        client.set_agent_name(&config.name);
        // Trim thresholds: stamp this agent's configured trigger/target
        // percentages of the n_ctx window (default 90/50) on the shared
        // client so the char budgets follow its TrimConfig (S3).
        client.set_trim_pcts(
            config.trim_config.trim_trigger_pct as u64,
            config.trim_config.trim_target_pct as u64,
        );
        let (tool_manager, control_mailbox): (
            Arc<ToolManager>,
            Arc<Mutex<Vec<crate::agents::types::ControlRequest>>>,
        ) = {
            // S2: one control mailbox shared by the per-execution tools.
            // Always created (the `run_llm_loop` drain is a no-op when none
            // of the tools was injected).
            let control_mailbox = Arc::new(Mutex::new(Vec::new()));
            let tool_manager_arc = tool_manager.unwrap_or_else(|| {
                Arc::new(ToolManager::new(Arc::new(
                    crate::tools::registry::ToolRegistry::new(
                        vec![],
                        Arc::new(crate::tools::types::TracingToolLogger),
                    ),
                )))
            });
            let shared = tool_manager_arc.as_ref();
            let tm = if config.get_shell_config().shell_enabled {
                shared.with_shell_config(config.get_shell_config())
            } else {
                shared.without_shell()
            };
            let tm = if config.handoff_enabled {
                let tool = crate::tools::builtin::handoff::HandoffTool::new(
                    control_mailbox.clone(),
                    config.agents_dir.clone(),
                    config.agents_search_dirs.clone(),
                    config.handoff_targets.clone(),
                );
                tm.with_handoff_tool(tool)
            } else {
                tm.without_handoff()
            };
            let tm = if config.restart_enabled {
                let tool = crate::tools::builtin::restart::RestartTool::new(control_mailbox.clone());
                tm.with_restart_tool(tool)
            } else {
                tm.without_restart()
            };
            let tm = if config.hand_back_enabled && client.session_meta().parent_session_id.is_some()
            {
                let tool = crate::tools::builtin::hand_back::HandBackTool::new(
                    control_mailbox.clone(),
                );
                tm.with_hand_back_tool(tool)
            } else {
                tm.without_hand_back()
            };
            // S4a: agents with `session_note_enabled` (default true) get the
            // pinned-note tool, wired to the shared control mailbox.
            let tm = if config.session_note_enabled {
                let tool =
                    crate::tools::builtin::session_note::SessionNoteTool::new(
                        control_mailbox.clone(),
                    );
                tm.with_session_note_tool(tool)
            } else {
                tm
            };
            (Arc::new(tm), control_mailbox)
        };
        Agent {
            config,
            llm_client,
            tool_manager,
            event_tx,
            last_llm_call_at: Instant::now(),
            client,
            memory,
            messages: Vec::new(),
            agent_session_id,
            model_prices,
            trimming: ContextTrimming::new(),
            control_mailbox,
            run_stats: RunStats::default(),
            run_tokens_in: 0,
            run_tokens_out: 0,
            injection_rx: None,
        }
    }
}

impl Agent {
    /// Start building an agent (see [`AgentBuilder`] for the optional inputs
    /// and the per-agent policy `build` applies).
    pub fn builder(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        client: Arc<ChatClient>,
    ) -> AgentBuilder {
        AgentBuilder::new(config, llm_client, client)
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

    /// Drain the control requests (handoff / restart / hand_back / session
    /// note) queued by the per-execution tools since the last LLM round.
    /// Consumes the whole queue in one go (insertion order preserved).
    pub(crate) fn drain_control(&self) -> Vec<crate::agents::types::ControlRequest> {
        std::mem::take(&mut self.control_mailbox.lock().unwrap())
    }

    /// Get the messages from the last execution for memory extraction.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// I1: trajectory stats (tool calls, tool errors, verification attempts,
    /// 1a: per-tool histogram) of the last completed `run_llm_loop` — fed to
    /// the improver.
    pub fn run_stats(&self) -> RunStats {
        self.run_stats.clone()
    }

    /// 2b: prompt/completion tokens of the last completed `run_llm_loop`
    /// (read by the eval harness for its cost record).
    pub fn run_tokens(&self) -> (u64, u64) {
        (self.run_tokens_in, self.run_tokens_out)
    }

}


#[cfg(test)]
mod tests;
