use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing;

use super::config::AgentConfig;
use super::invocation_registry::AgentInvocationRegistry;
use super::llm_client::LlmClient;
use super::types::AgentId;
use crate::tools::ToolManager;
use crate::types::Message;

/// Maximum LLM iterations per loop (soft limit - agent can continue)
const LLM_ITERATION_LIMIT: u32 = 20;

/// Maximum verification attempts before giving up.
const MAX_VERIFICATION_ATTEMPTS: u32 = 2;

/// System prompt for tool-output verification.
static VERIFICATION_SYSTEM_PROMPT: &str =
    "You are verifying whether tool outputs answer the user's request. \
     Respond with exactly 'VERIFIED' if the outputs are correct and complete, \
     or 'NEEDS_FIX' followed by a brief explanation if something is wrong.";

/// A configurable agent that runs an LLM loop with tool calls.
///
/// Each agent has its own system prompt, allowed tools, and can invoke
/// other agents via the agent_call tool.
#[derive(Clone)]
pub struct Agent {
    id: AgentId,
    config: AgentConfig,
    llm_client: Arc<dyn LlmClient>,
    tool_manager: Arc<Mutex<ToolManager>>,
    invocation_registry: Arc<AgentInvocationRegistry>,
    event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
}

impl Agent {
    /// Create a new agent from config.
    pub fn new(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<Mutex<ToolManager>>,
        invocation_registry: Arc<AgentInvocationRegistry>,
        event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
    ) -> Self {
        Self {
            id: AgentId::generate(),
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            event_tx,
        }
    }

    /// Create an agent from config with an empty tool manager.
    pub fn from_config(
        config: AgentConfig,
        llm_client: Arc<dyn LlmClient>,
        invocation_registry: Arc<AgentInvocationRegistry>,
    ) -> Self {
        let tool_registry = Arc::new(crate::tools::registry::ToolRegistry::new(
            vec![],
            Arc::new(crate::tools::types::TracingToolLogger),
        ));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        Self::new(
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            None,
        )
    }

    fn send_event(&self, event: crate::types::AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }

    /// Execute a request with this agent.
    pub async fn execute(
        &self,
        request: &str,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }

        // Build system prompt
        let system_prompt = self.build_system_prompt();

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

        self.send_event(crate::types::AppEvent::AgentChainStarted {
            agent_name: self.config.name.clone(),
            depth: 0,
        });

        let result = self.run_llm_loop(&mut messages, cancel_token).await;

        match &result {
            Ok(response) => {
                self.send_event(crate::types::AppEvent::AgentChainCompleted {
                    agent_name: self.config.name.clone(),
                    result: response.clone(),
                    depth: 0,
                });
            }
            Err(e) => {
                self.send_event(crate::types::AppEvent::AgentChainError {
                    agent_name: self.config.name.clone(),
                    error: e.clone(),
                    depth: 0,
                });
            }
        }

        result
    }

    /// Build the system prompt for this agent.
    fn build_system_prompt(&self) -> String {
        let mut prompt = if self.config.system_prompt.is_empty() {
            format!("You are the '{}' agent. {}", self.config.name, self.config.description)
        } else {
            self.config.system_prompt.clone()
        };

        // Add available agents for delegation
        let available_agents = self.invocation_registry.names();
        if !available_agents.is_empty() {
            prompt.push_str("\n\nYou can delegate tasks to other agents using the agent_call tool. Available agents: ");
            prompt.push_str(&available_agents.join(", "));
        }

        prompt
    }

    /// Run the LLM loop: call LLM, execute tool calls if present, repeat.
    async fn run_llm_loop(
        &self,
        messages: &mut Vec<Message>,
        cancel_token: &CancellationToken,
    ) -> Result<String, String> {
        let mut verification_attempts = 0u32;
        let mut iteration_count = 0u32;
        let start = Instant::now();

        loop {
            if cancel_token.is_cancelled() {
                return Err("Cancelled".to_string());
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

            iteration_count += 1;

            // Hard cap on iterations to prevent runaway LLM loops
            if iteration_count > LLM_ITERATION_LIMIT {
                tracing::warn!(
                    "[AGENT] Agent '{}' exceeded iteration limit ({}), stopping",
                    self.config.name,
                    LLM_ITERATION_LIMIT
                );
                return Err(format!(
                    "Agent '{}' exceeded iteration limit of {}",
                    self.config.name, LLM_ITERATION_LIMIT
                ));
            }

            // Call LLM
            let response = match self.llm_client.complete(messages).await {
                Ok(r) => r,
                Err(e) => return Err(format!("LLM call failed: {}", e)),
            };

            // Check if response is a routing JSON object (agent delegation)
            if let Some((sub_agent, task)) = self.parse_routing_object(&response) {
                let is_truncated = !response.trim().ends_with('}') && response.contains("\"agent\"");
                if is_truncated {
                    tracing::warn!(
                        "[AGENT] LLM response appears truncated (incomplete JSON): len={}",
                        response.len()
                    );
                }
                // Check depth limit
                if messages.iter().filter(|m| m.role == "assistant").count() as u32 >= self.config.max_depth {
                    tracing::warn!(
                        "[AGENT] Max delegation depth ({}) reached, returning response directly",
                        self.config.max_depth
                    );
                    return Ok(response);
                }
                // Only allow delegation to agents that exist in the registry
                if self.invocation_registry.has(&sub_agent) {
                    tracing::info!(
                        "[AGENT] Delegated to sub-agent '{}' with task: {}",
                        sub_agent, task
                    );
                    let context = serde_json::Value::Object(serde_json::Map::new());
                    let sub_result = match self.invocation_registry.invoke(&sub_agent, &task, &context).await {
                        Ok(result) => result.output.to_string(),
                        Err(e) => format!("Error invoking {}: {}", sub_agent, e),
                    };
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: sub_result.clone(),
                        timestamp: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    continue;
                }
                // Agent not found — reject delegation
                tracing::warn!(
                    "[AGENT] LLM requested non-existent agent '{}', rejecting delegation",
                    sub_agent
                );
                messages.push(Message {
                    role: "system".to_string(),
                    content: format!(
                        "You requested to delegate to agent '{}', but that agent does not exist. Handle the task yourself.",
                        sub_agent
                    ),
                    timestamp: String::new(),
                    tool_calls: None,
                    tool_call_id: None,
                });
                continue;
            }

            // Check if response contains bash/code blocks that should be converted to tool calls
            if let Some(tool_calls) = self.extract_bash_as_tool_calls(&response) {
                for tool_call in tool_calls {
                    if cancel_token.is_cancelled() {
                        return Err("Cancelled".to_string());
                    }
                    let tool_name = tool_call.function.name.clone();
                    let tool_args = serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();
                    let tool_manager = self.tool_manager.lock().unwrap().clone();
                    let tool_result = tool_manager.execute(&tool_name, tool_args).await;
                    let result_str = match tool_result {
                        Ok(output) => format!("{}", output),
                        Err(e) => {
                            tracing::warn!("[AGENT] Tool '{}' failed: {}", tool_name, e);
                            format!("Error: {}", e)
                        }
                    };
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: format!("Tool call: {}({})", tool_name, tool_call.function.arguments),
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
                for tool_call in tool_calls {
                    if cancel_token.is_cancelled() {
                        return Err("Cancelled".to_string());
                    }

                    let tool_name = tool_call.function.name.clone();
                    let tool_args = serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();
                    let tool_manager = self.tool_manager.lock().unwrap().clone();
                    let tool_result = tool_manager.execute(&tool_name, tool_args).await;

                    let result_str = match tool_result {
                        Ok(output) => format!("{}", output),
                        Err(e) => {
                            tracing::warn!(
                                "[AGENT] Tool '{}' failed: {}",
                                tool_name,
                                e
                            );
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

            // No more tool calls — verify the outputs answer the user's request
            if verification_attempts >= MAX_VERIFICATION_ATTEMPTS {
                tracing::info!(
                    "[AGENT] Agent '{}' completed in {} iterations",
                    self.config.name,
                    iteration_count
                );
                return Ok(response);
            }
            verification_attempts += 1;

            let original_request = self.extract_original_request(messages);
            let verification_result = self.verify_tool_outputs(messages, &original_request).await;
            match verification_result {
                Ok(true) => {
                    tracing::info!(
                        "[AGENT] Agent '{}' completed in {} iterations (verified)",
                        self.config.name,
                        iteration_count
                    );
                    return Ok(response);
                }
                Ok(false) => {
                    tracing::warn!(
                        "[AGENT] Agent '{}' verification failed (attempt {}/{}), feeding feedback to LLM",
                        self.config.name,
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
                        "[AGENT] Agent '{}' verification error: {}, proceeding with response",
                        self.config.name,
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
    fn parse_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
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

        for (s, e) in &regions {
            if e > s {
                let json_str = &response[*s..*e];
                if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                    return Some(calls);
                }
            }
        }

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

    /// Check if the LLM response is a routing JSON object and extract agent+task.
    fn parse_routing_object(&self, response: &str) -> Option<(String, String)> {
        let candidate: Option<(String, String)> =
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

        let is_known = self.invocation_registry.has(&agent);
        if !is_known {
            tracing::debug!(
                "[AGENT] Routing candidate agent '{}' is not a known agent; treating response as a normal answer",
                agent
            );
            return None;
        }

        Some((agent, task))
    }

    /// Extract bash/code blocks from LLM responses and convert them to file_io tool calls.
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
                let cmd_line = if lines.len() > 1 && (lines[0] == "bash" || lines[0] == "sh" || lines[0] == "shell") {
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
    use crate::tools::types::TracingToolLogger;
    use super::super::traits::{AgentError, AgentInvocation};
    use super::super::types::{AgentMetadata, AgentResult, AgentType, TaskStatus};

    fn make_agent(name: &str) -> Agent {
        let config = AgentConfig {
            name: name.to_string(),
            ..Default::default()
        };
        let llm_client = Arc::new(NoopLlm);
        let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        let invocation_registry = Arc::new(AgentInvocationRegistry::new());
        Agent::new(config, llm_client, tool_manager, invocation_registry, None)
    }

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
    fn test_agent_creation() {
        let agent = make_agent("test");
        assert_eq!(agent.config.name, "test");
        assert!(!agent.id.to_string().is_empty());
    }

    #[test]
    fn test_parse_routing_object_known_agent() {
        let agent = make_agent("test");
        // Register "coder" as an invokable agent
        agent.invocation_registry.register("coder", Arc::new(MockInvokable::new("coder")));
        
        let response = r#"{"agent": "coder", "task": "write a file"}"#;
        let result = agent.parse_routing_object(response);
        assert!(result.is_some());
        let (agent_name, task) = result.unwrap();
        assert_eq!(agent_name, "coder");
        assert_eq!(task, "write a file");
    }

    #[test]
    fn test_parse_routing_object_unknown_agent() {
        let agent = make_agent("test");
        let response = r#"{"agent": "nonexistent", "task": "do something"}"#;
        assert!(agent.parse_routing_object(response).is_none());
    }

    struct MockInvokable {
        name: String,
    }
    impl MockInvokable {
        fn new(name: &str) -> Self { Self { name: name.to_string() } }
    }
    #[async_trait::async_trait]
    impl AgentInvocation for MockInvokable {
        async fn invoke(&self, _request: &str, _context: &serde_json::Value) -> Result<AgentResult, AgentError> {
            Ok(AgentResult {
                task_id: "test".to_string(),
                agent_id: self.name.clone(),
                agent_type: AgentType::General,
                status: TaskStatus::Completed,
                output: serde_json::json!("mock result"),
                summary: String::new(),
                duration_ms: 0,
                completed_at: None,
            })
        }
        fn metadata(&self) -> AgentMetadata {
            AgentMetadata {
                name: self.name.clone(),
                description: format!("Mock agent: {}", self.name),
                agent_type: AgentType::General,
                allowed_tools: vec![],
            }
        }
    }
}
