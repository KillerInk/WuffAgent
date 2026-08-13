use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing;

use super::config::AgentConfig;
use super::llm_client::LlmClient;
use super::registry::AgentRegistry;
use crate::tools::ToolManager;
use crate::types::Message;

// Re-export AgentChainEntry from sessions model
pub use crate::sessions::model::AgentChainEntry;

/// The top-level engine that executes agent-driven requests.
///
/// Routes requests to appropriate agents based on the registry,
/// with depth tracking and cancellation support.
pub struct AgentEngine {
    pub(super) registry: Arc<AgentRegistry>,
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) tool_manager: Arc<ToolManager>,
    pub(super) max_depth: u32,
}

impl AgentEngine {
    /// Create a new AgentEngine.
    pub fn new(
        registry: Arc<AgentRegistry>,
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<ToolManager>,
        max_depth: u32,
    ) -> Self {
        Self {
            registry,
            llm_client,
            tool_manager,
            max_depth,
        }
    }

    /// Main entry point: execute a user request through the agent system.
    ///
    /// Routes to the best-matching agent(s) and returns the final response.
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

        // If no agents, fall back to a general LLM call
        if enabled_agents.is_empty() {
            tracing::warn!("[AGENT ENGINE] No enabled agents, falling back to direct LLM call");
            return self.direct_llm_call(request, &cancel_token).await;
        }

        // Route the request to the best agent
        let routing_messages = vec![
            Message {
                role: "system".to_string(),
                content: routing_prompt.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: format!("Request: {}\n\nReturn JSON: {{\"agent\": \"<agent_name>\", \"task\": \"<task_description>\"}}", request),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let routing_response = match self.llm_client.complete(&routing_messages).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[AGENT ENGINE] Routing LLM call failed: {}", e);
                return self.direct_llm_call(request, &cancel_token).await;
            }
        };

        // Parse routing response to get agent name
        let agent_name = self.parse_routing_response(&routing_response, &enabled_agents);
        tracing::info!("[AGENT ENGINE] Routed to agent: {}", agent_name);

        // Execute with the selected agent
        self.execute_with_agent(request, &agent_name, 0, &cancel_token).await
    }

    /// Execute a request with a specific agent, with depth tracking.
    async fn execute_with_agent(
        &self,
        request: &str,
        agent_name: &str,
        depth: u32,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }

        if depth >= self.max_depth {
            tracing::warn!("[AGENT ENGINE] Max depth ({}) reached, falling back to direct LLM", self.max_depth);
            return self.direct_llm_call(request, cancel_token).await;
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

        // Build system prompt for this agent
        let system_prompt = if agent_config.system_prompt.is_empty() {
            format!("You are the '{}' agent. {}", agent_name, agent_config.description)
        } else {
            agent_config.system_prompt.clone()
        };

        // Prepare messages
        let mut messages = vec![
            Message {
                role: "system".to_string(),
                content: system_prompt,
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: request.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        // Execute tool calls if any tools are allowed
        if !agent_config.allowed_tools.is_empty() {
            // Filter tool definitions to only allowed tools
            let _tool_defs: Vec<_> = self.tool_manager
                .get_tool_definitions()
                .into_iter()
                .filter(|t| agent_config.allowed_tools.contains(&t.function.name))
                .collect();
        }

        // Run the LLM loop with tool calls
        let result = self.run_llm_loop(&mut messages, &agent_config, depth, cancel_token).await?;

        Ok(result)
    }

    /// Run the LLM loop: call LLM, execute tool calls if present, repeat.
    async fn run_llm_loop(
        &self,
        messages: &mut Vec<Message>,
        agent_config: &AgentConfig,
        depth: u32,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        let mut max_iterations = 20;
        let start = Instant::now();

        while max_iterations > 0 {
            if cancel_token.is_cancelled() {
                return Err("Cancelled".to_string());
            }

            // Check timeout
            if agent_config.task_timeout_ms > 0
                && start.elapsed().as_millis() > agent_config.task_timeout_ms as u128
            {
                return Err(format!(
                    "Agent '{}' timed out after {}ms",
                    agent_config.name, agent_config.task_timeout_ms
                ));
            }

            max_iterations -= 1;

            // Call LLM
            let response = match self.llm_client.complete(messages).await {
                Ok(r) => r,
                Err(e) => return Err(format!("LLM call failed: {}", e)),
            };

            // Check if response contains tool calls
            // In this simplified engine, we parse for tool call patterns
            if let Some(tool_calls) = self.parse_tool_calls(&response) {
                // Execute each tool call
                for tool_call in tool_calls {
                    if cancel_token.is_cancelled() {
                        return Err("Cancelled".to_string());
                    }

                    let tool_result = self
                        .tool_manager
                        .execute(&tool_call.function.name, serde_json::from_str(&tool_call.function.arguments).unwrap_or_default())
                        .await;

                    let result_str = match tool_result {
                        Ok(output) => {
                            let output_str = format!("{}", output);
                            output_str
                        }
                        Err(e) => format!("Error: {}", e),
                    };

                    // Add tool result to messages
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: format!("Tool call: {}({})", tool_call.function.name, tool_call.function.arguments),
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    messages.push(Message {
                        role: "tool".to_string(),
                        content: result_str,
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: Some(tool_call.id.clone()),
                    });
                }
                // Continue loop with tool results
                continue;
            }

            // No more tool calls, return the response
            return Ok(response);
        }

        Err("Maximum LLM iterations exceeded".to_string())
    }

    /// Parse tool calls from an LLM response.
    /// This is a simplified parser — in production you'd use structured tool call handling.
    fn parse_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        // Look for JSON tool call patterns in the response
        if let Some(start) = response.find('[') {
            if let Some(end) = response.rfind(']') {
                if end > start {
                    let json_str = &response[start..=end];
                    if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                        return Some(calls);
                    }
                }
            }
        }
        None
    }

    /// Parse the routing response to extract the agent name.
    fn parse_routing_response(&self, response: &str, enabled_agents: &[&AgentConfig]) -> String {
        // Try to parse as JSON
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(agent) = value.get("agent").and_then(|a| a.as_str()) {
                // Validate it's a known agent
                if enabled_agents.iter().any(|a| a.name == agent) {
                    return agent.to_string();
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
            return Err("Cancelled".to_string());
        }

        let messages = vec![
            Message {
                role: "user".to_string(),
                content: request.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        self.llm_client.complete(&messages).await
    }
}

/// A tool call from an LLM response.
#[derive(Clone, Debug, serde::Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ToolFunction,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ToolFunction {
    name: String,
    arguments: String,
}
