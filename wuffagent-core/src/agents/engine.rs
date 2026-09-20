use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

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
    pub(super) tool_manager: Arc<Mutex<ToolManager>>,
    pub(super) event_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub(super) client: Arc<crate::client::ChatClient>,
    pub(super) memory: Option<Arc<crate::memory::MemoryManager>>,
    /// Session ID for this agent engine's persistent conversation.
    pub(super) agent_session_id: Option<String>,
    /// Directory holding agent profile JSON files, so the chat path can
    /// resolve handoff targets (see `with_agents_dir`).
    pub(super) agents_dir: Option<std::path::PathBuf>,
    /// Additional agent profile dirs for the chat path's handoff target
    /// resolution (see `with_agents_search_dirs`) — same discovery dirs the
    /// UI agent selector scans.
    pub(super) agents_search_dirs: Vec<std::path::PathBuf>,
    /// Completed task count, shared across clones; throttles post-task work.
    tasks_completed: Arc<AtomicUsize>,
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
        tool_manager: Arc<Mutex<ToolManager>>,
        client: Arc<crate::client::ChatClient>,
    ) -> Self {
        Self {
            llm_client,
            tool_manager,
            event_tx: None,
            client,
            memory: None,
            agent_session_id: None,
            agents_dir: None,
            agents_search_dirs: Vec::new(),
            tasks_completed: Arc::new(AtomicUsize::new(0)),
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

    /// Attach a memory manager to the engine.
    pub fn with_memory(mut self, memory: Arc<crate::memory::MemoryManager>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Set the event transmitter for agent chain events.
    pub fn with_event_tx(mut self, tx: Arc<Mutex<mpsc::Sender<AppEvent>>>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Return a clone of the engine with a new LLM client.
    /// Used to create per-session engines with isolated conversation state.
    pub fn with_client(mut self, client: crate::client::ChatClient) -> Self {
        self.client = Arc::new(client);
        self
    }

    /// Return a clone of the engine with the LLM client's reasoning effort
    /// updated (used so the agent tool loop honors the current UI setting).
    pub fn with_reasoning_effort(self, effort: crate::types::ReasoningEffort) -> Self {
        let mut client = (*self.client).clone();
        client.set_reasoning_effort(effort);
        Self {
            client: Arc::new(client),
            ..self
        }
    }

    /// Set the session ID for this engine.
    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.agent_session_id = Some(session_id);
        self
    }

    /// Return the current agent session ID.
    pub fn agent_session_id(&self) -> Option<&str> {
        self.agent_session_id.as_deref()
    }

    /// Execute a chat request using the agent engine's tool pipeline with a
    /// custom system prompt (instead of an agent's own system prompt).
    /// This is the chat path — same native tool-calling loop as /plan but with
    /// the prompt provided by the selected agent profile in the UI.
    ///
    /// `image` is an optional `data:` URI for a user-attached image (see
    /// [`Agent::execute`]); it is recorded on the user message so the model
    /// sees it on this turn and in later turns of the conversation.
    pub async fn execute_with_tools(
        &self,
        request: &str,
        system_prompt: &str,
        tool_policy: &crate::client::pipeline::ChatToolPolicy,
        image: Option<&str>,
        cancel_token: &CancellationToken,
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
        // Carry over the profile's reasoning effort and trim config so the
        // chat agent behaves like the profile it came from.
        chat_config.reasoning_effort = tool_policy.reasoning_effort;
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

        // Keep a copy for the post-task improvement check (Agent::new takes ownership).
        let maintenance_config = chat_config.clone();

        let mut agent = Agent::new(
            chat_config,
            self.llm_client.clone(),
            self.tool_manager.clone(),
            self.event_tx.clone(),
            self.client.clone(),
            self.memory.clone(),
            self.agent_session_id.clone(),
        );

        let result = agent.execute(request, image, cancel_token).await;

        // Post-task: throttled LLM memory maintenance + optional self-improvement
        // suggestions. Both are opt-in via MemoryConfig and never fail the task.
        self.post_task_maintenance(&maintenance_config, request, &result).await;

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

        if memory.config().auto_improve {
            match crate::memory::suggest_improvements(
                &memory,
                agent_config,
                task,
                &task_result,
                &*self.llm_client,
            )
            .await
            {
                Ok(suggestions) if !suggestions.is_empty() => {
                    if let Some(tx) = &self.event_tx {
                        let _ = tx.lock().unwrap().send(AppEvent::ImprovementSuggested {
                            agent_name: agent_config.name.clone(),
                            suggestions,
                            session_id: self.agent_session_id.clone().unwrap_or_default(),
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("[AGENT] Improvement check failed: {}", e),
            }
        }
    }
}
