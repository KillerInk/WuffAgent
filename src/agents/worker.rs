use super::traits::{Agent, AgentError};
use super::types::AgentId;
use super::types::{AgentResult, AgentType, Task};

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
    personality: String,
}

impl GenericWorker {
    pub fn new(
        name: &str,
        description: &str,
        allowed_tools: Vec<String>,
        personality: &str,
    ) -> Self {
        Self {
            id: AgentId::generate(),
            name: name.to_string(),
            description: description.to_string(),
            allowed_tools,
            personality: personality.to_string(),
        }
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

        // The actual tool execution is delegated to the supervisor which has
        // access to the ToolManager. This worker returns a placeholder result
        // that the supervisor will enrich.
        Ok(AgentResult {
            task_id: task.id.clone(),
            agent_id: self.id.to_string(),
            agent_type: self.agent_type(),
            status: super::types::TaskStatus::Completed,
            output: serde_json::Value::Object(serde_json::Map::new()),
            summary: format!("Task '{}' completed by worker '{}'", task.description, self.name),
            needs_refinement: false,
            suggested_followup: vec![],
            duration_ms: start.elapsed().as_millis() as u64,
            completed_at: Some(chrono::Utc::now()),
        })
    }
}

impl Agent for GenericWorker {
    fn id(&self) -> &AgentId { &self.id }
    fn name(&self) -> &str { &self.name }
    fn role(&self) -> super::traits::AgentRole {
        super::traits::AgentRole::CustomWorker(self.name.clone())
    }
    fn instructions(&self) -> &str { &self.personality }
}
