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
    /// Completed task count, shared across clones; throttles post-task work.
    tasks_completed: Arc<AtomicUsize>,
}

/// Run the LLM memory-maintenance pass at most once every N completed tasks.
const MAINTENANCE_TASK_COOLDOWN: usize = 10;

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
            tasks_completed: Arc::new(AtomicUsize::new(0)),
        }
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
    pub async fn execute_with_tools(
        &self,
        request: &str,
        system_prompt: &str,
        tool_policy: &crate::client::pipeline::ChatToolPolicy,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }

        let mut chat_config = AgentConfig::default();
        chat_config.name = "chat".to_string();
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

        let result = agent.execute(request, cancel_token).await;

        // Post-task: throttled LLM memory maintenance + optional self-improvement
        // suggestions. Both are opt-in via MemoryConfig and never fail the task.
        self.post_task_maintenance(&maintenance_config, request, &result).await;

        result
    }

    /// Run post-task memory upkeep: the maintenance pass (when enabled and the
    /// entry count is high enough, at most once per `MAINTENANCE_TASK_COOLDOWN`
    /// tasks) and self-improvement suggestions (when `auto_improve` is on).
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
            match memory.run_maintenance().await {
                Ok(report) => tracing::info!("[AGENT] {}", report.summary),
                Err(e) => tracing::warn!("[AGENT] Memory maintenance failed: {}", e),
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
