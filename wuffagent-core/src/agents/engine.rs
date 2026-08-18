use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing;

use super::config::AgentConfig;
use super::llm_client::LlmClient;
use super::registry::AgentRegistry;
use super::supervisor::SupervisorAgent;
use super::planner::PlannerAgent;
use super::traits::ChatClientLike;
use crate::tools::ToolManager;
use crate::types::{AppEvent, Message};

/// System prompt for tool-output verification.
static VERIFICATION_SYSTEM_PROMPT: &str =
    "You are verifying whether tool outputs answer the user's request. \
     Respond with exactly 'VERIFIED' if the outputs are correct and complete, \
     or 'NEEDS_FIX' followed by a brief explanation if something is wrong.";

/// Maximum verification attempts before giving up.
const MAX_VERIFICATION_ATTEMPTS: u32 = 2;

// Re-export AgentChainEntry from sessions model
pub use crate::sessions::model::AgentChainEntry;

/// Maximum LLM iterations per loop (soft limit - agent can continue)
const LLM_ITERATION_LIMIT: u32 = 20;

/// The top-level engine that executes agent-driven requests.
///
/// Routes requests to appropriate agents based on the registry,
/// with depth tracking and cancellation support.
#[derive(Clone)]
pub struct AgentEngine {
    pub(super) registry: Arc<AgentRegistry>,
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) tool_manager: Arc<ToolManager>,
    pub(super) max_depth: u32,
    pub(super) event_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
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
            event_tx: None,
        }
    }

    /// Set the event transmitter for agent chain events.
    pub fn with_event_tx(mut self, tx: Arc<Mutex<mpsc::Sender<AppEvent>>>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    fn send_chain_event(&self, event: AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }

    /// Phase 7 (unification): run a request through the planner → supervisor →
    /// workers pipeline (the "plan" path), returning the merged output string.
    ///
    /// This reuses the engine's `LlmClient` (adapted to `ChatClientLike`) and
    /// builds an `EngineWorker`-backed supervisor, so the planned mode and the
    /// simple routing mode share one execution core. The simple path in
    /// [`AgentEngine::execute`] is unchanged.
    pub async fn execute_plan_mode(
        &self,
        request: &str,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }
        let chat = LlmClientAdapter(self.llm_client.clone());
        let worker_registry = Arc::new(crate::agents::worker_registry::WorkerRegistry::new());
        let engine_clone = self.clone();
        worker_registry.register("engine", move || {
            Box::new(crate::agents::engine_worker::EngineWorker::new(
                "engine",
                "Engine-backed worker",
                crate::agents::types::AgentType::General,
                vec!["file_io".to_string(), "calculation".to_string(), "agent_call".to_string()],
                "Execute the task using the available tools.",
                engine_clone.clone(),
            ))
        });
        let supervisor = Arc::new(SupervisorAgent::new(
            worker_registry,
            vec![],
            4,
            self.event_tx.clone(),
        ));
        let planner = PlannerAgent::new(chat);
        let pipeline = crate::agents::pipeline::AgentPipeline::new(planner, supervisor, self.event_tx.clone());
        let results = pipeline.execute(request).await.map_err(|e| e.to_string())?;
        let mut combined = String::new();
        for r in &results {
            if let serde_json::Value::String(s) = &r.output {
                combined.push_str(&s);
            } else {
                combined.push_str(&r.output.to_string());
            }
            combined.push('\n');
        }
        Ok(combined.trim().to_string())
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
                return self.direct_llm_call(request, cancel_token).await;
            }
        };

        // Parse routing response to get agent name
        let agent_name = self.parse_routing_response(&routing_response, &enabled_agents);
        tracing::info!("[AGENT ENGINE] Routed to agent: {}", agent_name);

        // Execute with the selected agent
        self.execute_with_agent(request, &agent_name, 0, cancel_token).await
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
            self.send_chain_event(AppEvent::AgentChainCancelled {
                agent_name: agent_name.to_string(),
            });
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

        // Emit chain started event
        self.send_chain_event(AppEvent::AgentChainStarted {
            agent_name: agent_name.to_string(),
            depth,
        });

        // Build system prompt for this agent
        let available_agents = self.registry.routing_prompt().to_string();
        let system_prompt = if agent_config.system_prompt.is_empty() {
            format!("You are the '{}' agent. {}\n\nAvailable agents for delegation (use only these names):\n{}", agent_name, agent_config.description, available_agents)
        } else {
            format!("{}\n\nAvailable agents for delegation (use only these names):\n{}", agent_config.system_prompt, available_agents)
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

        // Run the LLM loop with tool calls - will continue even after iteration limit
        let result = self.run_llm_loop(&mut messages, &agent_config, depth, cancel_token).await;

        match &result {
            Ok(response) => {
                self.send_chain_event(AppEvent::AgentChainCompleted {
                    agent_name: agent_name.to_string(),
                    result: response.clone(),
                    depth,
                });
            }
            Err(e) => {
                self.send_chain_event(AppEvent::AgentChainError {
                    agent_name: agent_name.to_string(),
                    error: e.clone(),
                    depth,
                });
            }
        }

        result
    }

    /// Run the LLM loop: call LLM, execute tool calls if present, repeat.
    /// Tool failures are returned directly to the LLM for correction.
    /// Continues working even after hitting iteration limit (no hard stop).
    async fn run_llm_loop(
        &self,
        messages: &mut Vec<Message>,
        agent_config: &AgentConfig,
        depth: u32,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        // Track verification attempts to avoid infinite loops
        let mut verification_attempts = 0u32;
        let mut iteration_count = 0u32;
        let start = Instant::now();

        loop {
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

            iteration_count += 1;

            // M2: hard cap on iterations. The old code only logged a warning
            // here and kept looping forever. Stop after LLM_ITERATION_LIMIT
            // iterations to prevent runaway LLM loops.
            if iteration_count > LLM_ITERATION_LIMIT {
                tracing::warn!(
                    "[AGENT ENGINE] Agent '{}' exceeded iteration limit ({}), stopping",
                    agent_config.name,
                    LLM_ITERATION_LIMIT
                );
                return Err(format!(
                    "Agent '{}' exceeded iteration limit of {}",
                    agent_config.name, LLM_ITERATION_LIMIT
                ));
            }

            // Call LLM
            let response = match self.llm_client.complete(messages).await {
                Ok(r) => r,
                Err(e) => return Err(format!("LLM call failed: {}", e)),
            };

            // Check if response is a routing JSON object (agent delegation)
            if let Some((sub_agent, task)) = self.parse_routing_object(&response) {
                // Check if the response appears to be truncated (incomplete JSON)
                let is_truncated = !response.trim().ends_with('}') && response.contains("\"agent\"");
                if is_truncated {
                    tracing::warn!(
                        "[AGENT ENGINE] LLM response appears truncated (incomplete JSON): len={}, ends_with: {:?}",
                        response.len(),
                        response.trim().chars().last()
                    );
                }
                // Only allow delegation to agents that actually exist in the registry
                if self.registry.get_agent(&sub_agent).is_some() {
                    tracing::info!(
                        "[AGENT ENGINE] LLM delegated to sub-agent '{}' with task: {}",
                        sub_agent, task
                    );
                    let sub_result = Box::pin(self.execute_with_agent(&task, &sub_agent, depth + 1, cancel_token)).await?;
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: sub_result.clone(),
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    continue;
                }
                // Agent not found — reject delegation and tell LLM to use valid agent names
                tracing::warn!(
                    "[AGENT ENGINE] LLM requested non-existent agent '{}', rejecting delegation",
                    sub_agent
                );
                messages.push(Message {
                    role: "system".to_string(),
                    content: format!(
                        "You requested to delegate to agent '{}', but that agent does not exist. You must ONLY use agent names from this list: {}. Handle the task yourself or use a valid agent name.",
                        sub_agent,
                        self.registry.available_agent_names()
                    ),
                    timestamp: String::new(),
                    tool_calls: None,
                    tool_call_id: None,
                });
                continue;
            }

            // Check if response contains bash/code blocks that should be converted to file_io tool calls
            if let Some(tool_calls) = self.extract_bash_as_tool_calls(&response) {
                for tool_call in tool_calls {
                    if cancel_token.is_cancelled() {
                        return Err("Cancelled".to_string());
                    }
                    let tool_result = self.tool_manager
                        .execute(&tool_call.function.name, serde_json::from_str(&tool_call.function.arguments).unwrap_or_default())
                        .await;
                    let result_str = match tool_result {
                        Ok(output) => format!("{}", output),
                        Err(e) => {
                            tracing::warn!("[AGENT ENGINE] Tool '{}' failed: {}", tool_call.function.name, e);
                            format!("Error: {}", e)
                        }
                    };
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
                continue;
            }

            // Check if response contains tool calls
            if let Some(tool_calls) = self.parse_tool_calls(&response) {
                // Execute each tool call - failures are passed back to LLM
                for tool_call in tool_calls {
                    if cancel_token.is_cancelled() {
                        return Err("Cancelled".to_string());
                    }

                    let tool_result = self
                        .tool_manager
                        .execute(&tool_call.function.name, serde_json::from_str(&tool_call.function.arguments).unwrap_or_default())
                        .await;

                    // Pass result (success or failure) back to LLM
                    let result_str = match tool_result {
                        Ok(output) => {
                            let output_str = format!("{}", output);
                            output_str
                        }
                        Err(e) => {
                            tracing::warn!(
                                "[AGENT ENGINE] Tool '{}' failed: {}",
                                tool_call.function.name,
                                e
                            );
                            format!("Error: {}", e)
                        }
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
                // Continue loop with tool results - LLM will try to fix errors
                continue;
            }

            // No more tool calls — verify the outputs answer the user's request
            if verification_attempts >= MAX_VERIFICATION_ATTEMPTS {
                tracing::warn!(
                    "[AGENT ENGINE] Agent '{}' verification limit ({}) reached, returning response",
                    agent_config.name,
                    MAX_VERIFICATION_ATTEMPTS
                );
                return Ok(response);
            }
            verification_attempts += 1;

            let original_request = self.extract_original_request(messages);
            let verification_result = self.verify_tool_outputs(messages, &original_request).await;
            match verification_result {
                Ok(true) => {
                    tracing::info!(
                        "[AGENT ENGINE] Agent '{}' completed in {} iterations (verified)",
                        agent_config.name,
                        iteration_count
                    );
                    return Ok(response);
                }
                Ok(false) => {
                    tracing::warn!(
                        "[AGENT ENGINE] Agent '{}' verification failed (attempt {}/{}), feeding feedback to LLM",
                        agent_config.name,
                        verification_attempts,
                        MAX_VERIFICATION_ATTEMPTS
                    );
                    messages.push(Message {
                        role: "system".to_string(),
                        content: "The previous tool outputs did not fully satisfy the user's request. Please try again with corrected tool calls.".to_string(),
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        "[AGENT ENGINE] Agent '{}' verification error: {}, proceeding with response",
                        agent_config.name,
                        e
                    );
                    return Ok(response);
                }
            }
        }
    }

    /// Extract the original user request from the message history.
    fn extract_original_request(&self, messages: &[Message]) -> String {
        messages
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    /// Verify that the tool outputs in the message history satisfy the user's request.
    async fn verify_tool_outputs(
        &self,
        messages: &[Message],
        original_request: &str,
    ) -> Result<bool, String> {
        // Collect recent tool outputs for context
        let tool_outputs: Vec<String> = messages
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.content.clone())
            .collect();

        let recent_tool_summary: String = if tool_outputs.is_empty() {
            "No tool calls were made.".to_string()
        } else {
            tool_outputs
                .iter()
                .map(|o| o.chars().take(200).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };

        let verification_messages = vec![
            Message {
                role: "system".to_string(),
                content: VERIFICATION_SYSTEM_PROMPT.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: format!(
                    "User request: {}\n\nRecent tool outputs:\n{}\n\nIs the request satisfied?",
                    original_request, recent_tool_summary
                ),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let response = match self.llm_client.complete(&verification_messages).await {
            Ok(r) => r,
            Err(e) => return Err(format!("Verification LLM call failed: {}", e)),
        };

        Ok(response.to_uppercase().contains("VERIFIED") && !response.to_uppercase().contains("NEEDS_FIX"))
    }

    /// Parse tool calls from an LLM response.
    /// Uses proper JSON parsing with error recovery.
    ///
    /// M1: instead of taking only the FIRST bracket region (which may be a
    /// prose example that fails to parse), scan every top-level `[...]` region
    /// in order and return the first one that actually deserializes to
    /// `Vec<ToolCall>`.
    fn parse_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        // Collect every top-level bracket region (start..end byte offsets).
        let mut regions: Vec<(usize, usize)> = Vec::new();
        let mut depth = 0i32;
        let mut start: Option<usize> = None;
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
                '[' if !in_string => {
                    if depth == 0 {
                        start = Some(i);
                    }
                    depth += 1;
                }
                ']' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s) = start {
                            regions.push((s, i + 1));
                        }
                        start = None;
                    }
                }
                _ => {}
            }
        }

        // Try each region in order; the first that parses as a tool-call array wins.
        for (s, e) in &regions {
            if e > s {
                let json_str = &response[*s..*e];
                if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                    return Some(calls);
                }
            }
        }

        // Fallback: try to extract JSON from markdown code blocks
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

    /// Parse the routing response to extract the agent name.
    fn parse_routing_response(&self, response: &str, enabled_agents: &[&AgentConfig]) -> String {
        // Try to parse as JSON
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(agent) = value.get("agent").and_then(|a| a.as_str()) {
                // L1: use the fallback resolver (exact, then case-insensitive,
                // then substring, then generalist/first-enabled) so a slightly
                // misspelled agent name still routes to the right agent.
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

    /// Check if the LLM response is a routing JSON object and extract agent+task.
    /// Handles both raw JSON and markdown-wrapped JSON (```json ... ```).
    /// Returns Some((agent_name, task)) if the response looks like a delegation
    /// instruction.
    ///
    /// H5 guard: to avoid false-positives on ordinary JSON-structured answers
    /// that happen to contain "agent" and "task" keys, the named agent must be
    /// a real, enabled agent in the registry. Otherwise the response is treated
    /// as a normal answer (returns None).
    fn parse_routing_object(&self, response: &str) -> Option<(String, String)> {
        // Extract a candidate (agent, task) pair from the response.
        let candidate: Option<(String, String)> =
            // First try direct JSON parsing
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
                    // Fallback: try to extract JSON from markdown code blocks
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

        // Guard: only treat as delegation if `agent` names a known enabled
        // agent. A JSON answer that merely contains agent/task keys is not a
        // delegation instruction.
        let is_known = self
            .registry
            .get_all_agents()
            .iter()
            .any(|a| a.enabled && a.name == agent);
        if !is_known {
            tracing::debug!(
                "[AGENT ENGINE] Routing candidate agent '{}' is not a known enabled agent; treating response as a normal answer",
                agent
            );
            return None;
        }

        Some((agent, task))
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

        let messages = vec![
            Message {
                role: "user".to_string(),
                content: request.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
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

    /// Extract bash/code blocks from LLM responses and convert them to file_io tool calls.
    /// e.g. ```bash\nls -la /path/to/dir\n``` becomes a file_io list call.
    fn extract_bash_as_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        let mut calls = Vec::new();
        let mut id_counter = 0u32;

        // Find all markdown code blocks
        let mut rest = response;
        while let Some(start) = rest.find("```") {
            rest = &rest[start + 3..];
            if let Some(end) = rest.find("```") {
                let block = &rest[..end];
                rest = &rest[end + 3..];

                // Skip if it's a JSON block (already handled by parse_tool_calls or parse_routing_object)
                if block.trim().starts_with('{') || block.trim().starts_with('[') {
                    continue;
                }

                // Extract the command (first line after language hint)
                let lines: Vec<&str> = block.lines().collect();
                let cmd_line = if lines.len() > 1 && (lines[0] == "bash" || lines[0] == "sh" || lines[0] == "shell") {
                    lines[1..].join("\n").trim().to_string()
                } else {
                    block.trim().to_string()
                };

                if cmd_line.is_empty() {
                    continue;
                }

                // H4: parse the command, stripping flag tokens (starting with
                // '-') so they are not mistaken for file paths, and supporting
                // multiple file arguments (e.g. `cat a b`, `rm x y`).
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

                // Helper: emit a file_io call and bump the counter.
                let mut emit = |action: &str, value: &str| {
                    let args = serde_json::to_string(
                        &serde_json::json!({ "action": action, "path": value }),
                    )
                    .unwrap_or_default();
                    calls.push(ToolCall {
                        id: format!("call_{}", id_counter),
                        _call_type: "function".to_string(),
                        function: ToolFunction { name: "file_io".to_string(), arguments: args },
                    });
                    id_counter += 1;
                };

                let cmd = raw_parts[0];
                match cmd {
                    "ls" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        emit("list", path);
                    }
                    "cat" => {
                        for path in &args_parts {
                            emit("read", path);
                        }
                    }
                    "find" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        calls.push(ToolCall {
                            id: format!("call_{}", id_counter),
                            _call_type: "function".to_string(),
                            function: ToolFunction {
                                name: "file_io".to_string(),
                                arguments: serde_json::to_string(
                                    &serde_json::json!({ "action": "glob", "path": path, "pattern": "*" }),
                                )
                                .unwrap_or_default(),
                            },
                        });
                        id_counter += 1;
                    }
                    "pwd" => {
                        emit("file_info", ".");
                    }
                    "mkdir" => {
                        if let Some(path) = args_parts.first() {
                            let recursive = raw_parts.iter().any(|p| *p == "-p" || *p == "--parents");
                            let args = serde_json::to_string(
                                &serde_json::json!({ "action": "mkdir", "path": path, "recursive": recursive }),
                            )
                            .unwrap_or_default();
                            calls.push(ToolCall {
                                id: format!("call_{}", id_counter),
                                _call_type: "function".to_string(),
                                function: ToolFunction { name: "file_io".to_string(), arguments: args },
                            });
                            id_counter += 1;
                        }
                    }
                    "rm" => {
                        for path in &args_parts {
                            emit("delete", path);
                        }
                    }
                    "cp" => {
                        if args_parts.len() >= 2 {
                            let args = serde_json::to_string(
                                &serde_json::json!({ "action": "copy", "src": args_parts[0], "dest": args_parts[1] }),
                            )
                            .unwrap_or_default();
                            calls.push(ToolCall {
                                id: format!("call_{}", id_counter),
                                _call_type: "function".to_string(),
                                function: ToolFunction { name: "file_io".to_string(), arguments: args },
                            });
                            id_counter += 1;
                        }
                    }
                    "mv" => {
                        if args_parts.len() >= 2 {
                            let args = serde_json::to_string(
                                &serde_json::json!({ "action": "move", "src": args_parts[0], "dest": args_parts[1] }),
                            )
                            .unwrap_or_default();
                            calls.push(ToolCall {
                                id: format!("call_{}", id_counter),
                                _call_type: "function".to_string(),
                                function: ToolFunction { name: "file_io".to_string(), arguments: args },
                            });
                            id_counter += 1;
                        }
                    }
                    "head" | "tail" => {
                        // `head -n 20 file` -> file is the last non-flag arg.
                        if let Some(path) = args_parts.last() {
                            emit("read", path);
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

/// Adapts an [`LlmClient`] to the [`ChatClientLike`] trait so the planner can
/// reuse the engine's LLM client in plan mode.
struct LlmClientAdapter(Arc<dyn LlmClient>);

#[async_trait::async_trait]
impl ChatClientLike for LlmClientAdapter {
    async fn send_message(&self, messages: &[Message]) -> Result<String, String> {
        self.0.complete(messages).await
    }
    async fn send_streaming(&self, messages: &[Message]) -> Result<String, String> {
        self.0
            .stream(messages, Box::new(|_| {}))
            .await
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
    use crate::tools::lib::TracingToolLogger;

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
        AgentEngine::new(Arc::new(registry), Arc::new(NoopLlm), Arc::new(ToolManager::new(tool_registry)), 5)
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

    /// H4: `ls -la /path` must yield path `/path` (the flag is stripped), not
    /// `-la`.
    #[test]
    fn test_h4_ls_strips_flag() {
        let engine = engine_with_agents(&[]);
        let response = "```bash\nls -la /tmp/foo\n```";
        let calls = engine.extract_bash_as_tool_calls(response).unwrap();
        assert_eq!(calls.len(), 1, "exactly one call expected");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["action"], "list");
        assert_eq!(args["path"], "/tmp/foo", "flag must not be taken as the path");
    }

    /// H4: `cat file1 file2` must produce one read call per file.
    #[test]
    fn test_h4_cat_multiple_files() {
        let engine = engine_with_agents(&[]);
        let response = "```bash\ncat a.txt b.txt\n```";
        let calls = engine.extract_bash_as_tool_calls(response).unwrap();
        assert_eq!(calls.len(), 2, "one read per file");
        let p0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        let p1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(p0["path"], "a.txt");
        assert_eq!(p1["path"], "b.txt");
    }

    /// H4: `rm foo bar` must delete both files (the old code deleted only `bar`).
    #[test]
    fn test_h4_rm_multiple_files() {
        let engine = engine_with_agents(&[]);
        let response = "```bash\nrm foo bar\n```";
        let calls = engine.extract_bash_as_tool_calls(response).unwrap();
        assert_eq!(calls.len(), 2, "one delete per file");
        let p0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        let p1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(p0["path"], "foo");
        assert_eq!(p1["path"], "bar");
    }

    /// H5: a JSON answer that contains "agent"+"task" keys but names an agent
    /// that is NOT registered must NOT be treated as a delegation.
    #[test]
    fn test_h5_unknown_agent_not_delegation() {
        let engine = engine_with_agents(&["general"]);
        let response = r#"{"agent": "nonexistent_bot", "task": "do something"}"#;
        assert!(
            engine.parse_routing_object(response).is_none(),
            "unknown agent must not be treated as a delegation"
        );
    }

    /// H5: a JSON object naming a KNOWN enabled agent IS a delegation.
    #[test]
    fn test_h5_known_agent_is_delegation() {
        let engine = engine_with_agents(&["general"]);
        let response = r#"{"agent": "general", "task": "do something"}"#;
        let parsed = engine.parse_routing_object(response).unwrap();
        assert_eq!(parsed.0, "general");
        assert_eq!(parsed.1, "do something");
    }
}
