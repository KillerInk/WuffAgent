use std::collections::HashMap;
use std::sync::Arc;

use crate::tools::lib::{Tool, ToolOutput, ToolParams, ToolSchema, ToolError};
use crate::agents::invocation_registry::AgentInvocationRegistry;
use crate::agents::types::{AgentCallParams, Task};

/// A tool that allows workers to invoke other agents.
///
/// When a worker calls this tool, it spawns the target agent to execute
/// the specified sub-task and returns the result.
pub struct AgentCallTool {
    registry: Arc<AgentInvocationRegistry>,
    /// Shared tokio runtime to avoid creating a new one for each call.
    rt: std::sync::Mutex<Option<tokio::runtime::Runtime>>,
}

impl AgentCallTool {
    pub fn new(registry: Arc<AgentInvocationRegistry>) -> Self {
        Self {
            registry,
            rt: std::sync::Mutex::new(None),
        }
    }

    /// Get or create a shared tokio runtime.
    fn get_runtime(&self) -> Result<tokio::runtime::Handle, ToolError> {
        let mut rt_guard = self.rt.lock().map_err(|e| ToolError::Execution(format!("Failed to lock runtime: {}", e)))?;
        
        if rt_guard.is_none() {
            *rt_guard = Some(tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| ToolError::Execution(format!("Failed to create runtime: {}", e)))?);
        }
        
        // We need to get a handle to the runtime. Since we can't easily get a handle
        // from a owned Runtime, we'll use a different approach: spawn on the current
        // runtime if available, otherwise create a new one.
        drop(rt_guard);
        
        // Try to get the current runtime handle
        tokio::runtime::Handle::try_current()
            .map_err(|_| ToolError::Execution("No tokio runtime available".to_string()))
    }

    /// Parse parameters from tool input.
    fn parse_params(&self, params: &ToolParams) -> Result<AgentCallParams, ToolError> {
        let target = params
            .get::<String>("target")
            .ok_or_else(|| ToolError::InvalidParams("Missing required field: target".to_string()))?;

        let task = params
            .get::<String>("task")
            .ok_or_else(|| ToolError::InvalidParams("Missing required field: task".to_string()))?;

        let input = params
            .values
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        Ok(AgentCallParams { target, task, input })
    }
}

impl Tool for AgentCallTool {
    fn name(&self) -> &str {
        "agent_call"
    }

    fn description(&self) -> &str {
        "Invoke another agent to execute a sub-task. Returns the agent's result as JSON."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "agent_call".to_string(),
            description: "Invoke another agent to execute a sub-task".to_string(),
            input_type: Some(crate::tools::lib::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "target".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Name of the agent to invoke".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "task".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Description of the sub-task to execute".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "input".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "object".to_string(),
                            description: "Input parameters for the sub-task (optional)".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["target".to_string(), "task".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::lib::ToolResult<ToolOutput> {
        let call_params = self.parse_params(&params)?;

        // Create a task for the target agent
        let task = Task {
            id: format!("agent-call-{}", uuid::Uuid::new_v4()),
            description: call_params.task,
            agent_type: crate::agents::types::AgentType::General,
            input: call_params.input,
            depends_on: None,
            max_retries: 1,
            priority: 0,
            metadata: serde_json::json!({ "invoked_by_tool": true }),
        };

        // Empty context for tool-invoked calls
        let context = serde_json::Value::Object(serde_json::Map::new());

        // Try to use the current runtime if available
        let registry = self.registry.clone();
        let task_clone = task.clone();
        let context_clone = context.clone();
        let target = call_params.target.clone();

        // Check if we're already in a tokio runtime
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // We're in a runtime, spawn the task
            let join_handle = handle.spawn(async move {
                match registry.invoke(&target, &task_clone, &context_clone).await {
                    Ok(result) => Ok(ToolOutput::Success(result.output)),
                    Err(e) => Ok(ToolOutput::Error(e.to_string())),
                }
            });
            
            // Block on the spawned task
            return handle.block_on(join_handle)
                .map_err(|e| ToolError::Execution(format!("Join error: {}", e)))?;
        }

        // No runtime available, create a temporary one
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ToolError::Execution(format!("Failed to create runtime: {}", e)))?;
        rt.block_on(async move {
            match registry.invoke(&target, &task_clone, &context_clone).await {
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

        let call_params = tool.parse_params(&params).unwrap();
        assert_eq!(call_params.target, "researcher");
        assert_eq!(call_params.task, "Search for X");
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
}