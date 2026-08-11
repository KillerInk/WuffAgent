use std::sync::Arc;

use tokio::sync::Mutex;

use crate::tools::{ToolManager, ToolParams, ToolOutput};
use super::super::types::{AgentResult, AgentType, Task, TaskStatus};
use super::super::traits::{Agent, AgentError};
use super::super::types::AgentId;
use super::super::WorkerAgent;

/// A worker that executes research-oriented tasks using web_search and file analysis.
pub struct ResearchWorker {
    id: AgentId,
    name: String,
    description: String,
    agent_type: AgentType,
    allowed_tools: Vec<String>,
    personality: String,
    tool_manager: Arc<Mutex<ToolManager>>,
}

impl ResearchWorker {
    pub fn new(
        name: &str,
        description: &str,
        agent_type: AgentType,
        allowed_tools: Vec<String>,
        personality: &str,
        tool_manager: Arc<Mutex<ToolManager>>,
    ) -> Self {
        Self {
            id: AgentId::generate(),
            name: name.to_string(),
            description: description.to_string(),
            agent_type,
            allowed_tools,
            personality: personality.to_string(),
            tool_manager,
        }
    }
}

#[async_trait::async_trait]
impl WorkerAgent for ResearchWorker {
    fn agent_type(&self) -> AgentType {
        self.agent_type.clone()
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

        tracing::info!(
            "ResearchWorker '{}' running task '{}'",
            self.name,
            task.description
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
            let tool_name = self.allowed_tools.first()
                .ok_or_else(|| AgentError::TaskFailure(
                    format!("Worker '{}' has no allowed tools", self.name)
                ))?
                .clone();
        }

        // Build tool parameters from task input
        let params = build_tool_params(task, context);

        // Execute the tool
        let tool_manager = self.tool_manager.lock().await;
        let result = tool_manager.execute(&tool_name, params).await;
        drop(tool_manager);

        match result {
            Ok(ToolOutput::Success(output)) => {
                let duration = start.elapsed().as_millis() as u64;
                tracing::info!(
                    "ResearchWorker '{}' completed task '{}' in {}ms",
                    self.name,
                    task.description,
                    duration
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type.clone(),
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
                let duration = start.elapsed().as_millis() as u64;
                tracing::warn!(
                    "ResearchWorker '{}' task '{}' returned error: {}",
                    self.name,
                    task.description,
                    err
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type.clone(),
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
                let duration = start.elapsed().as_millis() as u64;
                tracing::warn!(
                    "ResearchWorker '{}' failed task '{}': {}",
                    self.name,
                    task.description,
                    e
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type.clone(),
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

impl Agent for ResearchWorker {
    fn id(&self) -> &AgentId { &self.id }
    fn name(&self) -> &str { &self.name }
    fn role(&self) -> crate::agents::traits::AgentRole {
        crate::agents::traits::AgentRole::CustomWorker(self.name.clone())
    }
    fn instructions(&self) -> &str { &self.personality }
}

impl ResearchWorker {
    /// Determine which tool to use for this task.
    fn determine_tool(&self, task: &Task) -> String {
        if let Some(tool) = task.input.get("tool").and_then(|t| t.as_str()) {
            return tool.to_string();
        }
        if let Some(action) = task.input.get("action").and_then(|a| a.as_str()) {
            return action.to_string();
        }
        // Default to web_search for research workers
        self.allowed_tools.first()
            .cloned()
            .unwrap_or_else(|| "web_search".to_string())
    }
}

/// Build ToolParams from task input and context.
fn build_tool_params(task: &Task, context: &serde_json::Value) -> ToolParams {
    let mut params = ToolParams::new();

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

    params
}
