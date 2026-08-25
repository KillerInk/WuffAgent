use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

use tokio_util::sync::CancellationToken;
use tracing;

use super::agent::Agent;
use super::config::AgentConfig;
use super::invocation_registry::AgentInvocationRegistry;
use super::llm_client::LlmClient;
use super::registry::AgentRegistry;
use crate::tools::ToolManager;
use crate::types::{AppEvent, Message};

// Re-export AgentChainEntry from sessions model
pub use crate::sessions::model::AgentChainEntry;

/// The top-level engine that executes agent-driven requests.
///
/// Routes requests to appropriate agents based on the registry,
/// with depth tracking and cancellation support.
#[derive(Clone)]
pub struct AgentEngine {
    pub(super) registry: Arc<AgentRegistry>,
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) tool_manager: Arc<Mutex<ToolManager>>,
    pub(super) event_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub(super) client: Arc<crate::client::ChatClient>,
    pub(super) invocation_registry: Arc<AgentInvocationRegistry>,
    pub(super) memory: Option<Arc<crate::memory::MemoryManager>>,
}

impl AgentEngine {
    /// Create a new AgentEngine.
    pub fn new(
        registry: Arc<AgentRegistry>,
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<Mutex<ToolManager>>,
        client: Arc<crate::client::ChatClient>,
        invocation_registry: Arc<AgentInvocationRegistry>,
    ) -> Self {
        Self {
            registry,
            llm_client,
            tool_manager,
            event_tx: None,
            client,
            invocation_registry,
            memory: None,
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

    /// Return a clone of the engine with the LLM client's reasoning effort
    /// updated (used so the agent tool loop honors the current UI setting).
    pub fn with_reasoning_effort(mut self, effort: crate::types::ReasoningEffort) -> Self {
        let mut client = (*self.client).clone();
        client.set_reasoning_effort(effort);
        self.client = Arc::new(client);
        self
    }

    /// Return the comma-separated list of enabled agent names.
    pub fn available_agent_names(&self) -> Option<String> {
        let names = self.registry.available_agent_names();
        if names.is_empty() {
            None
        } else {
            Some(names)
        }
    }

    fn send_chain_event(&self, event: AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }

    /// Main entry point: execute a user request through the agent system.
    ///
    /// Routes to the best-matching agent and returns the final response.
    pub async fn execute(
        &self,
        request: &str,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        tracing::info!("[AGENT ENGINE] Executing request: {}", request);

        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }

        // Build the routing prompt (cached in registry)
        let routing_prompt = self.registry.routing_prompt().to_string();

        // Get all enabled agents for routing
        let enabled_agents: Vec<&AgentConfig> = self
            .registry
            .get_all_agents()
            .into_iter()
            .filter(|a| a.enabled)
            .collect();

        // If no agents, fall back to a direct LLM call
        if enabled_agents.is_empty() {
            tracing::warn!("[AGENT ENGINE] No enabled agents, falling back to direct LLM call");
            return self.direct_llm_call(request, cancel_token).await;
        }

        // Route the request to the best agent
        let routing_messages = vec![
            Message {
                role: "system".to_string(),
                content: routing_prompt.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: "user".to_string(),
                content: format!("Request: {}\n\nReply with just the agent name (one of: {}).", request, enabled_agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];

        let routing_response = match self.llm_client.complete(&routing_messages).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[AGENT ENGINE] Routing LLM call failed: {}", e);
                return self.direct_llm_call(request, cancel_token).await;
            }
        };

        // Parse routing response to get agent name
        let agent_name = self.parse_routing_response(&routing_response, &enabled_agents);
        tracing::info!("[AGENT ENGINE] Routed to agent: {}", agent_name);

        // Execute with the selected agent
        self.execute_with_agent(request, &agent_name, cancel_token).await
    }

    /// Execute a request with a specific agent.
    async fn execute_with_agent(
        &self,
        request: &str,
        agent_name: &str,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            self.send_chain_event(AppEvent::AgentChainCancelled {
                agent_name: agent_name.to_string(),
            });
            return Err("Cancelled".to_string());
        }

        // Get agent config
        let agent_config = match self.registry.get_agent(agent_name) {
            Some(cfg) => cfg.clone(),
            None => {
                tracing::warn!("[AGENT ENGINE] Agent '{}' not found, using general agent", agent_name);
                match self.registry.get_agent("general") {
                    Some(cfg) => cfg.clone(),
                    None => {
                        return self.direct_llm_call(request, cancel_token).await;
                    }
                }
            }
        };

        // Create the agent and execute — use the shared invocation registry
        let memory = self.memory.clone();
        let agent_config_for_improve = agent_config.clone();
        let agent = Agent::new(
            agent_config,
            self.llm_client.clone(),
            self.tool_manager.clone(),
            self.invocation_registry.clone(),
            self.event_tx.clone(),
            self.client.clone(),
            memory,
        );

        let result = agent.execute(request, cancel_token).await;

        // Post-task: extract memories and suggest improvements
        if let Some(memory) = &self.memory {
            // Collect messages from the chain entries if available
            let _ = memory.extract_and_save(&vec![], "agent_task").await;
            
            // Suggest improvements if auto_improve is enabled
            if memory.config().auto_improve {
                let result_str = match &result {
                    Ok(r) => r.clone(),
                    Err(e) => format!("Error: {}", e),
                };
                let suggestions = memory.suggest_improvements(&agent_config_for_improve, request, &result_str).await;
                if let Ok(suggestions) = suggestions {
                    if !suggestions.is_empty() {
                        self.send_chain_event(crate::types::AppEvent::ImprovementSuggested {
                            agent_name: agent_name.to_string(),
                            suggestions,
                        });
                    }
                }
            }
        }

        result
    }

    /// Parse the routing response to extract the agent name.
    fn parse_routing_response(&self, response: &str, enabled_agents: &[&AgentConfig]) -> String {
        // Try to parse as JSON
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(agent) = value.get("agent").and_then(|a| a.as_str()) {
                let resolved = self.registry.find_fallback_agent(agent);
                if enabled_agents.iter().any(|a| a.name == resolved) {
                    return resolved;
                }
            }
        }

        // Fallback: use simple keyword matching
        let response_lower = response.to_lowercase();
        for agent in enabled_agents {
            if response_lower.contains(&agent.name.to_lowercase()) {
                return agent.name.clone();
            }
        }

        // Default to first enabled agent
        enabled_agents
            .first()
            .map(|a| a.name.clone())
            .unwrap_or_else(|| "general".to_string())
    }

    /// Direct LLM call fallback when no agents are available or routing fails.
    async fn direct_llm_call(
        &self,
        request: &str,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            self.send_chain_event(AppEvent::AgentChainCancelled {
                agent_name: "general".to_string(),
            });
            return Err("Cancelled".to_string());
        }

        // Include a system prompt so the fallback call still has context.
        let system_prompt = self
            .registry
            .get_agent("general")
            .map(|a| a.system_prompt.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| {
                "You are a helpful assistant.".to_string()
            });
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system_prompt,
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: "user".to_string(),
                content: request.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];

        let result = self.llm_client.complete(&messages).await;

        match &result {
            Ok(response) => {
                self.send_chain_event(AppEvent::AgentChainCompleted {
                    agent_name: "general".to_string(),
                    result: response.clone(),
                    depth: 0,
                });
            }
            Err(e) => {
                self.send_chain_event(AppEvent::AgentChainError {
                    agent_name: "general".to_string(),
                    error: e.clone(),
                    depth: 0,
                });
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::types::TracingToolLogger;

    /// Build a minimal engine backed by an empty tool registry and the given
    /// set of enabled agent names.
    fn engine_with_agents(names: &[&str]) -> AgentEngine {
        let mut registry = AgentRegistry::new();
        for name in names {
            registry.add_agent_for_test(AgentConfig {
                name: name.to_string(),
                ..Default::default()
            });
        }
        let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        let invocation_registry = Arc::new(AgentInvocationRegistry::new());
        AgentEngine::new(
            Arc::new(registry),
            Arc::new(NoopLlm),
            Arc::new(Mutex::new(crate::tools::ToolManager::new(tool_registry))),
            Arc::new(crate::client::ChatClient::new("http://localhost:1")),
            invocation_registry,
        )
    }

    /// A no-op LLM client (never actually called by the parsing tests).
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
    fn test_parse_routing_response_json() {
        let engine = engine_with_agents(&["general", "coder"]);
        let response = r#"{"agent": "coder"}"#;
        let agent = engine.parse_routing_response(response, &engine.registry.get_all_agents().into_iter().filter(|a| a.enabled).collect::<Vec<_>>());
        assert_eq!(agent, "coder");
    }

    #[test]
    fn test_parse_routing_response_fallback() {
        let engine = engine_with_agents(&["general", "coder"]);
        let response = "Use the coder agent please";
        let agents: Vec<&AgentConfig> = engine.registry.get_all_agents().into_iter().filter(|a| a.enabled).collect();
        let agent = engine.parse_routing_response(response, &agents);
        assert_eq!(agent, "coder");
    }

    #[test]
    fn test_parse_routing_response_default() {
        let engine = engine_with_agents(&["general"]);
        let response = "something else entirely";
        let agents: Vec<&AgentConfig> = engine.registry.get_all_agents().into_iter().filter(|a| a.enabled).collect();
        let agent = engine.parse_routing_response(response, &agents);
        assert_eq!(agent, "general");
    }
}
