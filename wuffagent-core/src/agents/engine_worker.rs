use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::engine::AgentEngine;
use super::traits::{Agent, AgentError, AgentRole, WorkerAgent};
use super::types::{AgentId, AgentResult, AgentType, Task, TaskStatus};

/// A [`WorkerAgent`] that delegates task execution to an [`AgentEngine`].
///
/// Phase 6 (unification): the supervisor/planner pipeline is built on the
/// `WorkerAgent` trait, whereas the production backend (iced/egui) drives the
/// `AgentEngine`. This bridge lets the pipeline execute each task *through* the
/// engine (routing, LLM tool-loop, bash extraction, verification) instead of
/// through a fixed single tool, so there is one execution path for both the
/// conversational and the planned modes.
#[derive(Clone)]
pub struct EngineWorker {
    id: AgentId,
    name: String,
    description: String,
    agent_type: AgentType,
    allowed_tools: Vec<String>,
    system_prompt: String,
    engine: AgentEngine,
}

impl EngineWorker {
    /// Wrap an [`AgentEngine`] as a worker that handles the given agent type.
    pub fn new(
        name: &str,
        description: &str,
        agent_type: AgentType,
        allowed_tools: Vec<String>,
        system_prompt: &str,
        engine: AgentEngine,
    ) -> Self {
        Self {
            id: AgentId::generate(),
            name: name.to_string(),
            description: description.to_string(),
            agent_type,
            allowed_tools,
            system_prompt: system_prompt.to_string(),
            engine,
        }
    }
}

#[async_trait::async_trait]
impl WorkerAgent for EngineWorker {
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
            "EngineWorker '{}' running task '{}' through AgentEngine",
            self.name,
            task.description
        );

        // Build the request from the task description plus any input fields and
        // the dependency context, so the engine's LLM sees the full picture.
        let mut request = task.description.clone();
        if let serde_json::Value::Object(obj) = &task.input {
            for (k, v) in obj {
                request.push_str(&format!("\n{}: {}", k, v));
            }
        }
        if let Some(dep_id) = &task.depends_on {
            if let Some(parent_output) = context.get(dep_id) {
                request.push_str(&format!("\nparent({}): {}", dep_id, parent_output));
            }
        }

        let cancel = CancellationToken::new();
        let engine_output = self.engine.execute(&request, &cancel).await;

        let duration = start.elapsed().as_millis() as u64;
        match engine_output {
            Ok(output) => {
                tracing::info!(
                    "EngineWorker '{}' completed task '{}' in {}ms",
                    self.name,
                    task.description,
                    duration
                );
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type.clone(),
                    status: TaskStatus::Completed,
                    output: serde_json::json!({ "result": output }),
                    summary: format!("Task '{}' completed by '{}'", task.description, self.name),
                    needs_refinement: false,
                    fixable: false,
                    suggested_followup: vec![],
                    duration_ms: duration,
                    completed_at: Some(chrono::Utc::now()),
                })
            }
            Err(e) => {
                tracing::warn!(
                    "EngineWorker '{}' failed task '{}': {}",
                    self.name,
                    task.description,
                    e
                );
                let fixable = e.contains("is required") || e.contains("required");
                Ok(AgentResult {
                    task_id: task.id.clone(),
                    agent_id: self.id.to_string(),
                    agent_type: self.agent_type.clone(),
                    status: TaskStatus::Failed,
                    output: serde_json::json!({ "error": e }),
                    summary: format!("Task '{}' failed: {}", task.description, e),
                    needs_refinement: false,
                    fixable,
                    suggested_followup: vec![],
                    duration_ms: duration,
                    completed_at: Some(chrono::Utc::now()),
                })
            }
        }
    }
}

impl Agent for EngineWorker {
    fn id(&self) -> &AgentId {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn role(&self) -> AgentRole {
        AgentRole::CustomWorker(self.name.clone())
    }
    fn instructions(&self) -> &str {
        &self.system_prompt
    }
}

/// Convenience: build an [`EngineWorker`] from an engine and the agent type
/// name used across the codebase.
pub fn engine_worker_for(
    name: &str,
    agent_type: AgentType,
    engine: AgentEngine,
) -> Arc<dyn WorkerAgent> {
    Arc::new(EngineWorker::new(
        name,
        &format!("Engine-backed worker for {}", name),
        agent_type,
        vec!["file_io".to_string(), "calculation".to_string(), "agent_call".to_string()],
        "Execute the task using the available tools.",
        engine,
    ))
}
