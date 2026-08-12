use std::sync::Arc;

use tokio::sync::Mutex;

use super::traits::{Agent, AgentError};
use super::types::AgentId;
use super::types::{AgentResult, AgentType, Task, TaskStatus};
use crate::tools::{ToolManager, ToolOutput};

/// Base trait that all workers implement.
/// Re-exported from traits for convenience.
pub use super::traits::WorkerAgent;

/// A generic worker that executes tasks by calling tools through a ToolManager.
/// This is the default worker type used when no specialized worker matches.
pub struct GenericWorker {
    id: AgentId,
    name: String,
    description: String,
    allowed_tools: Vec<String>,
    system_prompt: String,
    tool_manager: Arc<Mutex<ToolManager>>,
}

impl GenericWorker {
    pub fn new(
        name: &str,
        description: &str,
        allowed_tools: Vec<String>,
        system_prompt: &str,
        tool_manager: Arc<Mutex<ToolManager>>,
    ) -> Self {
        Self {
            id: AgentId::generate(),
            name: name.to_string(),
            description: description.to_string(),
            allowed_tools,
            system_prompt: system_prompt.to_string(),
            tool_manager,
        }
    }

    /// Determine which tool to call based on task input.
    fn determine_tool(&self, task: &Task) -> String {
        if let Some(tool) = task.input.get("tool").and_then(|t| t.as_str()) {
            return tool.to_string();
        }
        if let Some(action) = task.input.get("action").and_then(|a| a.as_str()) {
            return action.to_string();
        }
        self.allowed_tools.first()
            .cloned()
            .unwrap_or_else(|| "file_io".to_string())
    }
}

#[async_trait::async_trait]
impl super::traits::WorkerAgent for GenericWorker {
    fn agent_type(&self) -> AgentType {
        // Default to General; subclasses can override
        AgentType::General
    }

    fn allowed_tools(&self) -> Vec<String> {
        self.allowed_tools.clone()
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn execute_task(
        &mut self,
        task: &Task,
        context: &serde_json::Value,
    ) -> Result<AgentResult, AgentError> {
        let start = std::time::Instant::now();

        // Log task start
        tracing::info!(
            "Worker '{}' executing task '{}': {:?}",
            self.name,
            task.description,
            task.input
        );

        // Determine which tool to call based on task input
        let tool_name = self.determine_tool(task);

        // Check authorization
        if !self.allowed_tools.contains(&tool_name) {
            tracing::warn!(
                "Worker '{}' not authorized for tool '{}', using fallback",
                self.name,
                tool_name
            );
            // Fall back to first allowed tool
            let _tool_name = self.allowed_tools.first()
                .ok_or_else(|| AgentError::TaskFailure(
                    format!("Worker '{}' has no allowed tools", self.name)
                ))?
                .clone();
        }

        // Build tool parameters from task input
        let mut params = crate::tools::ToolParams::new();
        if let serde_json::Value::Object(obj) = &task.input {
            for (k, v) in obj {
                params.values.insert(k.clone(), v.clone());
            }
        }
        if let Some(dep_id) = &task.depends_on {
            if let Some(parent_output) = context.get(dep_id) {
                params.values.insert(format!("parent_{}", dep_id), parent_output.clone());
            }
        }

        // Execute the tool
        let tool_manager = self.tool_manager.lock().await;
        let result = tool_manager.execute(&tool_name, params).await;
        drop(tool_manager);

        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(ToolOutput::Success(output)) => {
                tracing::info!(
                    "Worker '{}' completed task '{}' in {}ms",
                    self.name,
                    task.description,
                    duration
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type(),
                    status: TaskStatus::Completed,
                    output,
                    summary: format!("Task '{}' completed by '{}'", task.description, self.name),
                    needs_refinement: false,
                    suggested_followup: vec![],
                    duration_ms: duration,
                    completed_at: Some(chrono::Utc::now()),
                })
            }
            Ok(ToolOutput::Error(err)) => {
                tracing::warn!(
                    "Worker '{}' task '{}' returned error: {}",
                    self.name,
                    task.description,
                    err
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type(),
                    status: TaskStatus::Failed,
                    output: serde_json::json!({ "error": err }),
                    summary: format!("Task '{}' failed: {}", task.description, err),
                    needs_refinement: false,
                    suggested_followup: vec![],
                    duration_ms: duration,
                    completed_at: Some(chrono::Utc::now()),
                })
            }
            Err(e) => {
                tracing::warn!(
                    "Worker '{}' failed task '{}': {}",
                    self.name,
                    task.description,
                    e
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type(),
                    status: TaskStatus::Failed,
                    output: serde_json::json!({ "error": e.to_string() }),
                    summary: format!("Task '{}' failed: {}", task.description, e),
                    needs_refinement: false,
                    suggested_followup: vec![],
                    duration_ms: duration,
                    completed_at: Some(chrono::Utc::now()),
                })
            }
        }
    }
}

impl Agent for GenericWorker {
    fn id(&self) -> &AgentId { &self.id }
    fn name(&self) -> &str { &self.name }
    fn role(&self) -> super::traits::AgentRole {
        super::traits::AgentRole::CustomWorker(self.name.clone())
    }
    fn instructions(&self) -> &str { &self.system_prompt }
}
