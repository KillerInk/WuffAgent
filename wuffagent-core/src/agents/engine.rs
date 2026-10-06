use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

use super::agent::Agent;
use super::config::AgentConfig;
use super::LlmClient;
use crate::tools::ToolManager;
use crate::types::AppEvent;

/// The top-level engine that executes agent-driven requests.
#[derive(Clone)]
pub struct AgentEngine {
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) tool_manager: Arc<ToolManager>,
    pub(super) client: Arc<crate::client::ChatClient>,
    pub(super) memory: Option<Arc<crate::memory::MemoryManager>>,
    /// Directory holding agent profile JSON files, so the chat path can
    /// resolve handoff targets (see `with_agents_dir`).
    pub(super) agents_dir: Option<std::path::PathBuf>,
    /// Additional agent profile dirs for the chat path's handoff target
    /// resolution (see `with_agents_search_dirs`) — same discovery dirs the
    /// UI agent selector scans.
    pub(super) agents_search_dirs: Vec<std::path::PathBuf>,
    /// Completed task count, shared across clones; throttles post-task work.
    tasks_completed: Arc<AtomicUsize>,
    /// 1b: the app config's model price table (cost_usd estimates on run
    /// lines; empty = all runs "recorded but unpriced").
    model_prices: Vec<crate::config::ModelPrice>,
    /// Shared LLM-activity tracker (status bar; None = no activity events,
    /// e.g. tests): attached to built agents and labels post-task
    /// improvement checks.
    activity: Option<Arc<crate::activity::ActivityTracker>>,
}

/// Run-scoped values for one `execute_with_tools` call, passed by the caller
/// (the chat pipeline) instead of traveling on the engine.
///
/// Previously these rode on a per-run engine clone chain
/// (`with_event_tx` → `with_reasoning_mode` → `with_session_id` →
/// `with_injection_channel`); passing them per call lets the one shared
/// engine serve every run in a session.
#[derive(Default)]
pub struct RunParams {
    /// UI event channel for this run's chain events (None = headless).
    pub event_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    /// Session ID for event routing + the persisted conversation (None = none).
    pub session_id: Option<String>,
    /// Reasoning-effort selection for the run: Auto follows the selected
    /// agent profile's own effort; Explicit forces a level.
    pub reasoning: crate::types::ReasoningMode,
    /// Mid-run injection channel (UI → agent loop), if one was created for
    /// this run: the loop drains it at LLM round boundaries, and
    /// `execute_with_tools` drains the remainder after the loop ends.
    pub injection: Option<Arc<Mutex<mpsc::Receiver<crate::sessions::QueuedMessage>>>>,
    /// Run-scoped per-call tool-cancellation registry: every in-flight tool
    /// call registers its own token here keyed `"{session_id}:{call_id}"`,
    /// so the UI can stop ONE live tool card (the shell's process is killed).
    /// None = the agent keeps a private registry (headless: nothing can
    /// address individual calls).
    pub tool_cancel: Option<Arc<crate::tools::cancel::CancelRegistry>>,
}

/// Run an LLM memory-maintenance step at most once every N completed tasks.
/// Each step is a single small batch (see `memory_maintenance_batch_size`),
/// so it stays cheap enough to run fairly often; repeated steps make forward
/// progress on the oldest entries of the store.
const MAINTENANCE_TASK_COOLDOWN: usize = 3;

impl AgentEngine {
    /// Create a new AgentEngine.
    pub fn new(
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<ToolManager>,
        client: Arc<crate::client::ChatClient>,
    ) -> Self {
        Self {
            llm_client,
            tool_manager,
            client,
            memory: None,
            agents_dir: None,
            agents_search_dirs: Vec::new(),
            tasks_completed: Arc::new(AtomicUsize::new(0)),
            model_prices: Vec::new(),
            activity: None,
        }
    }

    /// Set the agents directory used by the chat path to resolve handoff
    /// targets (agent profile JSON files). Without it, handoff target
    /// resolution finds no profiles.
    pub fn with_agents_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.agents_dir = Some(dir);
        self
    }

    /// Set additional directories (beyond the primary agents dir) that the
    /// chat path scans when resolving `handoff` targets. Without these,
    /// profiles that live in project-level `agents/` dirs are invisible to
    /// the handoff tool.
    pub fn with_agents_search_dirs(mut self, dirs: Vec<std::path::PathBuf>) -> Self {
        self.agents_search_dirs = dirs;
        self
    }

    /// 1b: the app config's model price table (cost_usd estimates on run
    /// lines; empty = all runs "recorded but unpriced").
    pub fn with_model_prices(mut self, model_prices: Vec<crate::config::ModelPrice>) -> Self {
        self.model_prices = model_prices;
        self
    }

    /// Attach a memory manager to the engine.
    pub fn with_memory(mut self, memory: Arc<crate::memory::MemoryManager>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Shared LLM-activity tracker (status bar): attached to every agent the
    /// engine builds and used to label post-task improvement checks
    /// (None = no activity events).
    pub fn with_activity(
        mut self,
        activity: Arc<crate::activity::ActivityTracker>,
    ) -> Self {
        self.activity = Some(activity);
        self
    }

    /// Point the engine at a shared chat client. The per-session engine is
    /// built once with the session's single `Arc<ChatClient>` — the session,
    /// the engine and the pipeline all share that one client (the
    /// conversation store is isolated per session, shared within it).
    pub fn with_client(mut self, client: Arc<crate::client::ChatClient>) -> Self {
        self.client = client;
        self
    }

    /// Execute a chat request using the agent engine's tool pipeline with a
    /// custom system prompt (instead of an agent's own system prompt).
    /// This is the chat path — same native tool-calling loop as /plan but with
    /// the prompt provided by the selected agent profile in the UI.
    ///
    /// `image` is an optional `data:` URI for a user-attached image (see
    /// [`Agent::execute`]); it is recorded on the user message so the model
    /// sees it on this turn and in later turns of the conversation.
    ///
    /// `params` carries the run-scoped values (event channel, session ID,
    /// reasoning-effort mode, mid-run injection channel); callers without a
    /// live UI or a per-run channel pass `&RunParams::default()`.
    pub async fn execute_with_tools(
        &self,
        request: &str,
        system_prompt: &str,
        tool_policy: &crate::types::ChatToolPolicy,
        image: Option<&str>,
        cancel_token: &CancellationToken,
        params: &RunParams,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }

        let mut chat_config = AgentConfig::default();
        // Use the profile's name so handoff markers read naturally
        // ("[Handoff from 'architect' to 'coder']") and the model's system
        // prompt identity matches the selected profile.
        chat_config.name = tool_policy.agent_name.clone();
        chat_config.system_prompt = system_prompt.to_string();
        chat_config.task_timeout_ms = 0; // no timeout for chat
                                         // Apply the selected profile's tool policy. An empty allowed_tools means
                                         // the chat agent gets all available tools (the old default); the shell
                                         // config lets a profile like "coder" restrict the shell to its allowlist.
        chat_config.allowed_tools = tool_policy.allowed_tools.clone();
        chat_config.shell_config = tool_policy.shell_config.clone();
        // Trim config comes from the profile. The run's wire level lives on
        // the run client (see `run_client` below), so `Off` here makes
        // `Agent::builder` reuse that client as-is — one client clone per
        // run, not the old `with_reasoning_mode` clone + builder re-clone.
        chat_config.reasoning_effort = crate::types::ReasoningEffort::Off;
        chat_config.trim_config = tool_policy.trim_config.clone();
        // Handoff: the profile's flag/targets gate the `handoff` tool, and the
        // agents dir is where target profiles are resolved from.
        chat_config.handoff_enabled = tool_policy.handoff_enabled;
        chat_config.handoff_targets = tool_policy.handoff_targets.clone();
        chat_config.restart_enabled = tool_policy.restart_enabled;
        chat_config.agents_dir = self
            .agents_dir
            .clone()
            .unwrap_or_else(crate::agents::config::default_agents_dir);
        chat_config.agents_search_dirs = self.agents_search_dirs.clone();

        // Keep a copy for the post-task improvement check (Agent::builder takes ownership).
        let maintenance_config = chat_config.clone();

        // Per-run client handle: at most ONE deep clone per run, carrying the
        // run's final wire level — Auto → the selected profile's own effort
        // (Off there = nothing forced: reuse the shared client, whose level
        // the session keeps in sync with the mode); Explicit(e) → e.
        let run_client: Arc<crate::client::ChatClient> = match params.reasoning {
            crate::types::ReasoningMode::Explicit(e) => {
                let c = (*self.client).clone();
                c.set_reasoning_effort(e);
                Arc::new(c)
            }
            crate::types::ReasoningMode::Auto
                if tool_policy.reasoning_effort != crate::types::ReasoningEffort::Off =>
            {
                let c = (*self.client).clone();
                c.set_reasoning_effort(tool_policy.reasoning_effort);
                Arc::new(c)
            }
            crate::types::ReasoningMode::Auto => Arc::clone(&self.client),
        };

        let mut agent_builder = Agent::builder(chat_config, self.llm_client.clone(), run_client)
            .tool_manager(self.tool_manager.clone())
            .event_tx(params.event_tx.clone())
            .memory(self.memory.clone())
            .agent_session_id(params.session_id.clone())
            .model_prices(self.model_prices.clone());
        // Status bar: attach the shared activity tracker (the agent's default
        // label `agent: <name>` applies).
        if let Some(t) = &self.activity {
            agent_builder = agent_builder.activity(t.clone());
        }
        let mut agent = agent_builder.build();
        // Mid-run injection channel: the agent loop drains it at LLM round
        // boundaries (user messages sent while this run is active are
        // injected into the current turn as soon as the model can see them).
        if let Some(rx) = &params.injection {
            agent = agent.with_injection_channel(Arc::clone(rx));
        }
        // Per-call tool cancellation (live tool card's Stop button): the
        // chat pipeline created a run-scoped registry and keeps it to cancel
        // individual calls by `"{session_id}:{call_id}"` key.
        if let Some(registry) = &params.tool_cancel {
            agent = agent.with_tool_cancel_registry(Arc::clone(registry));
        }

        let result = agent.execute(request, image, cancel_token).await;

        // User messages that arrived after the agent loop had already ended
        // (e.g. during the final verification call) can no longer reach the
        // model this turn — hand them back to the UI to run as the next turn.
        // Runs BEFORE post-task maintenance so the UI is not kept waiting.
        if let Some(rx) = &params.injection {
            let rx = rx.lock().unwrap();
            while let Ok(message) = rx.try_recv() {
                if let Some(tx) = &params.event_tx {
                    let _ = tx.lock().unwrap().send(AppEvent::UserMessageDrained {
                        message: Box::new(message),
                        session_id: params.session_id.clone().unwrap_or_default(),
                    });
                }
            }
        }

        // Post-task: throttled LLM memory maintenance + optional self-improvement
        // suggestions. Both are opt-in via MemoryConfig and never fail the task.
        self.post_task_maintenance(
            &maintenance_config,
            request,
            &result,
            agent.run_stats(),
            params,
        )
        .await;

        result
    }

    /// Run post-task memory upkeep: a single maintenance STEP (when enabled
    /// and the entry count is high enough, at most once per
    /// `MAINTENANCE_TASK_COOLDOWN` tasks) and self-improvement suggestions
    /// (when `auto_improve` is on).
    async fn post_task_maintenance(
        &self,
        agent_config: &AgentConfig,
        task: &str,
        result: &std::result::Result<String, String>,
        stats: crate::agents::RunStats,
        params: &RunParams,
    ) {
        let memory = match &self.memory {
            Some(m) => m.clone(),
            None => return,
        };
        let task_result = match result {
            Ok(r) => r.clone(),
            Err(e) => format!("(task failed: {})", e),
        };

        let completed = self.tasks_completed.fetch_add(1, Ordering::SeqCst) + 1;
        if memory.config().memory_maintenance
            && memory.count() >= memory.config().memory_maintenance_threshold
            && completed % MAINTENANCE_TASK_COOLDOWN == 0
        {
            // The step is internally bounded by `memory_maintenance_timeout_secs`
            // (per batch) inside `run_maintenance_step`, so no outer timeout is
            // needed — a hung LLM call can't hold task completion hostage.
            match memory.run_maintenance_step().await {
                Ok(report) => tracing::info!("[AGENT] {}", report.summary),
                Err(e) => tracing::warn!("[AGENT] Memory maintenance step failed: {}", e),
            }
        }

        // 2a (per-agent improvement state): `auto_improve` defaults ON, but
        // the LLM call only runs when THIS agent's per-task cooldown has
        // elapsed (with no-op-streak backoff) AND new lesson evidence for
        // THIS agent has arrived since its last check — a busy agent can no
        // longer starve an idle one's checks, and an agent with no new
        // lessons is not re-checked.
        let mconfig = memory.config();
        if mconfig.auto_improve {
            let name = agent_config.name.clone();
            memory.record_agent_task_completed(&name);
            if memory.agent_improvement_due(&name, mconfig.improvement_cooldown_tasks)
                && memory.has_new_agent_improvement_evidence(&name)
            {
                // Status bar: label the improvement check's LLM call
                // ("improvement"; no tracker → plain client, e.g. tests).
                let imp_llm = crate::activity::LabeledLlm::wrap(
                    Arc::clone(&self.llm_client),
                    self.activity.clone(),
                    "improvement",
                    None,
                );
                let imp_client: &dyn LlmClient = match &imp_llm {
                    Some(l) => l.as_ref(),
                    None => self.llm_client.as_ref(),
                };
                let produced =
                    match crate::memory::suggest_improvements(
                        &memory,
                        agent_config,
                        task,
                        &task_result,
                        &stats,
                        imp_client,
                    )
                    .await
                    {
                        Ok(suggestions) if !suggestions.is_empty() => {
                            if let Some(tx) = &params.event_tx {
                                let _ = tx.lock().unwrap().send(AppEvent::ImprovementSuggested {
                                    agent_name: name.clone(),
                                    suggestions,
                                    session_id: params.session_id.clone().unwrap_or_default(),
                                });
                            }
                            true
                        }
                        Ok(_) => false,
                        Err(e) => {
                            tracing::warn!("[AGENT] Improvement check failed: {}", e);
                            false
                        }
                    };
                // Record the check after the attempt (even on Err/empty
                // result): new evidence since this timestamp re-arms the
                // next check; an empty result extends the no-op streak.
                memory.record_agent_improvement_check(&name, produced);
            }
        }
    }
}

#[cfg(test)]
mod tests;
