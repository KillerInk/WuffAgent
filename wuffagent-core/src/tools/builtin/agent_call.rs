use std::collections::HashMap;
use std::sync::Arc;

use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema, ToolError};
use crate::agents::invocation_registry::AgentInvocationRegistry;

/// A tool that allows agents to invoke other agents.
///
/// When an agent calls this tool, it invokes the target agent to execute
/// the specified sub-task and returns the result.
///
/// The `allowed_targets` list restricts which agents may be invoked. An
/// empty list means unrestricted (the default global registration).
pub struct AgentCallTool {
    registry: Arc<AgentInvocationRegistry>,
    allowed_targets: Vec<String>,
}

impl AgentCallTool {
    /// Create an unrestricted tool (any registered agent may be invoked).
    pub fn new(registry: Arc<AgentInvocationRegistry>) -> Self {
        Self {
            registry,
            allowed_targets: Vec::new(),
        }
    }

    /// Create a tool that may only invoke the given agent names.
    pub fn with_allowlist(
        registry: Arc<AgentInvocationRegistry>,
        allowed_targets: Vec<String>,
    ) -> Self {
        Self {
            registry,
            allowed_targets,
        }
    }

    /// Check whether the target is permitted by this tool's allowlist.
    fn check_target(&self, target: &str) -> Result<(), ToolError> {
        if self.allowed_targets.is_empty() || self.allowed_targets.iter().any(|t| t == target) {
            Ok(())
        } else {
            Err(ToolError::InvalidParams(format!(
                "Agent '{}' is not invokable from this agent. Allowed agents: {}",
                target,
                self.allowed_targets.join(", ")
            )))
        }
    }

    /// Parse parameters from tool input.
    fn parse_params(&self, params: &ToolParams) -> Result<(String, String), ToolError> {
        let target = params
            .get::<String>("target")
            .ok_or_else(|| ToolError::InvalidParams("Missing required field: target".to_string()))?;

        let task = params
            .get::<String>("task")
            .ok_or_else(|| ToolError::InvalidParams("Missing required field: task".to_string()))?;

        Ok((target, task))
    }

    /// Parse parameters, also returning the optional input field for testing.
    #[cfg(test)]
    fn parse_params_with_input(&self, params: &ToolParams) -> Result<(String, String, serde_json::Value), ToolError> {
        let (target, task) = self.parse_params(params)?;
        let input = params
            .values
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        Ok((target, task, input))
    }
}

impl Tool for AgentCallTool {
    fn name(&self) -> &str {
        "agent_call"
    }

    fn description(&self) -> &str {
        "Invoke another agent to execute a sub-task. Returns the agent's result as JSON. \
         Prefer your own tools when a task is a single step — delegation spawns a full \
         sub-conversation and is more expensive."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "agent_call".to_string(),
            description: "Invoke another agent to execute a sub-task".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "target".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Name of the agent to invoke".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "task".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Description of the sub-task to execute".to_string(),
                            nullable: false,
                        },
                    );
                    map
                }),
                required: vec!["target".to_string(), "task".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let (target, task) = self.parse_params(&params)?;
        self.check_target(&target)?;

        // Empty context for tool-invoked calls
        let context = serde_json::Value::Object(serde_json::Map::new());
        let registry = self.registry.clone();
        let task_clone = task.clone();
        let context_clone = context.clone();

        // Check if we're already in a tokio runtime
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let cancel_token = tokio_util::sync::CancellationToken::new();
            let join_handle = handle.spawn(async move {
                match registry.invoke(&target, &task_clone, &context_clone, &cancel_token).await {
                    Ok(result) => Ok(ToolOutput::Success(result.output)),
                    Err(e) => Ok(ToolOutput::Error(e.to_string())),
                }
            });
            
            return handle.block_on(join_handle)
                .map_err(|e| ToolError::Execution(format!("Join error: {}", e)))?;
        }

        // No runtime available, create a temporary one
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ToolError::Execution(format!("Failed to create runtime: {}", e)))?;
        rt.block_on(async move {
            let cancel_token = tokio_util::sync::CancellationToken::new();
            match registry.invoke(&target, &task_clone, &context_clone, &cancel_token).await {
                Ok(result) => Ok(ToolOutput::Success(result.output)),
                Err(e) => Ok(ToolOutput::Error(e.to_string())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_params() {
        let registry = AgentInvocationRegistry::new();
        let tool = AgentCallTool::new(Arc::new(registry));

        let mut params = ToolParams::new();
        params.values.insert("target".to_string(), serde_json::json!("researcher"));
        params.values.insert("task".to_string(), serde_json::json!("Search for X"));
        params.values.insert("input".to_string(), serde_json::json!({ "query": "test" }));

        let (target, task, input) = tool.parse_params_with_input(&params).unwrap();
        assert_eq!(target, "researcher");
        assert_eq!(task, "Search for X");
        assert_eq!(input["query"], "test");
    }

    #[test]
    fn test_parse_params_missing_target() {
        let registry = AgentInvocationRegistry::new();
        let tool = AgentCallTool::new(Arc::new(registry));

        let mut params = ToolParams::new();
        params.values.insert("task".to_string(), serde_json::json!("Search for X"));

        let result = tool.parse_params(&params);
        assert!(result.is_err());
    }

    #[test]
    fn test_allowlist_allows_listed_target() {
        let registry = AgentInvocationRegistry::new();
        let tool = AgentCallTool::with_allowlist(
            Arc::new(registry),
            vec!["researcher".to_string(), "coder".to_string()],
        );
        tool.check_target("researcher").unwrap();
        tool.check_target("coder").unwrap();
    }

    #[test]
    fn test_allowlist_rejects_unlisted_target() {
        let registry = AgentInvocationRegistry::new();
        let tool = AgentCallTool::with_allowlist(Arc::new(registry), vec!["coder".to_string()]);
        let err = tool.check_target("executor").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("executor"), "error should name the rejected target: {}", msg);
        assert!(msg.contains("coder"), "error should list the allowed agents: {}", msg);
    }

    #[test]
    fn test_unrestricted_allows_any_target() {
        let registry = AgentInvocationRegistry::new();
        let tool = AgentCallTool::new(Arc::new(registry));
        tool.check_target("anything").unwrap();
    }
}
