use std::path::PathBuf;
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
    /// Directory where this agent engine's session files are stored.
    pub(super) agent_session_dir: PathBuf,
}

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
            agent_session_dir: PathBuf::new(),
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

    /// Set the agent session for this engine.
    pub fn set_agent_session(&mut self, session_id: Option<String>, session_dir: PathBuf) {
        self.agent_session_id = session_id;
        self.agent_session_dir = session_dir;
    }

    /// Return the current agent session ID.
    pub fn agent_session_id(&self) -> Option<&str> {
        self.agent_session_id.as_deref()
    }

    /// Return the current agent session dir.
    pub fn agent_session_dir(&self) -> &PathBuf {
        &self.agent_session_dir
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

        let mut agent = Agent::new(
            chat_config,
            self.llm_client.clone(),
            self.tool_manager.clone(),
            self.event_tx.clone(),
            self.client.clone(),
            self.memory.clone(),
            self.agent_session_id.clone(),
            self.agent_session_dir.clone(),
        );

        let result = agent.execute(request, cancel_token).await;

        result
    }
}
