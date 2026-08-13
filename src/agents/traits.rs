use crate::types::Message;

use super::types::{AgentId, AgentResult, AgentType, ExecutionPlan, Task};

/// Error type for agent operations.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Agent {0} not found")]
    AgentNotFound(String),
    #[error("Task execution failed: {0}")]
    TaskFailure(String),
    #[error("Plan generation failed: {0}")]
    PlanError(String),
    #[error("LLM call failed: {0}")]
    LlmError(String),
    #[error("Tool execution failed: {0}")]
    ToolError(#[from] crate::tools::lib::ToolError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Timeout after {0}ms")]
    Timeout(u64),
    #[error("Cancellation requested")]
    Cancelled,
    #[error("Worker configuration error: {0}")]
    ConfigError(String),
}

pub type AgentResultType<T> = Result<T, AgentError>;

/// All agents share a common interface for identification and lifecycle.
#[async_trait::async_trait]
pub trait Agent: Send + Sync {
    /// Unique identifier for this agent instance.
    fn id(&self) -> &AgentId;
    /// Human-readable name.
    fn name(&self) -> &str;
    /// The agent's role in the hierarchy.
    fn role(&self) -> AgentRole;
    /// System prompt or instructions for this agent.
    fn instructions(&self) -> &str;
}

/// The role an agent plays in the hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRole {
    Planner,
    Supervisor,
    Worker(AgentType),
    /// A user-configured custom worker.
    CustomWorker(String),
}

/// Trait for LLM clients that the Planner can use.
/// This allows mocking in tests.
#[async_trait::async_trait]
pub trait ChatClientLike: Send + Sync {
    async fn send_message(&self, messages: &[Message]) -> Result<String, String>;
    async fn send_streaming(&self, messages: &[Message]) -> Result<String, String>;
}

/// Planner Agent — generates and refines execution plans.
#[async_trait::async_trait]
pub trait PlannerAgent: Agent {
    /// Generate an execution plan from a user request.
    async fn generate_plan(
        &self,
        request: &str,
        context: Option<&serde_json::Value>,
    ) -> AgentResultType<ExecutionPlan>;

    /// Refine an existing plan based on feedback from the Supervisor.
    async fn refine_plan(
        &self,
        plan: &ExecutionPlan,
        completed: &[AgentResult],
        failed: &[AgentResult],
        context: &serde_json::Value,
    ) -> AgentResultType<ExecutionPlan>;

    /// Evaluate whether the current state satisfies the user's objective.
    /// Hybrid: mechanical check first, LLM as fallback.
    async fn is_objective_satisfied(
        &self,
        plan: &ExecutionPlan,
        results: &[AgentResult],
    ) -> AgentResultType<bool>;
}

/// Supervisor Decision — what to do after evaluating task results.
#[derive(Clone, Debug)]
pub enum SupervisorDecision {
    /// All tasks complete, return results to user.
    Complete { final_output: serde_json::Value },
    /// Send feedback to Planner for plan refinement.
    Refine {
        completed: Vec<AgentResult>,
        failed: Vec<AgentResult>,
        context: serde_json::Value,
    },
    /// Re-attempt failed tasks.
    Retry { tasks: Vec<Task> },
    /// Add new tasks suggested by workers.
    Continue { new_tasks: Vec<Task> },
    /// Tool parameter errors that the Planner can fix by regenerating the plan with correct params.
    NeedsFix {
        failed: Vec<AgentResult>,
        completed: Vec<AgentResult>,
        context: serde_json::Value,
    },
}

/// Supervisor Agent — spawns and coordinates Worker Agents.
#[async_trait::async_trait]
pub trait SupervisorAgent: Agent {
    /// Execute a plan by spawning and coordinating Worker Agents.
    /// Returns the final decision and accumulated results.
    async fn execute_plan(
        &self,
        plan: &ExecutionPlan,
    ) -> AgentResultType<(SupervisorDecision, Vec<AgentResult>)>;

    /// Spawn a Worker Agent for a specific task, selecting the best match.
    async fn spawn_worker(
        &self,
        task: &Task,
    ) -> AgentResultType<Box<dyn super::worker::WorkerAgent>>;

    /// Re-delegate a list of failed tasks to new worker instances.
    async fn retry_tasks(
        &self,
        tasks: &[Task],
        prior_results: &[AgentResult],
    ) -> AgentResultType<Vec<AgentResult>>;

    /// Decide whether to retry, refine, or complete.
    async fn decide_next_action(
        &self,
        results: &[AgentResult],
        plan: &ExecutionPlan,
    ) -> AgentResultType<SupervisorDecision>;
}

/// Worker Agent — executes a single task.
#[async_trait::async_trait]
pub trait WorkerAgent: Agent {
    /// Execute a single task and return the result.
    async fn execute_task(
        &mut self,
        task: &Task,
        context: &serde_json::Value,
    ) -> AgentResultType<AgentResult>;

    /// The agent type this worker handles.
    fn agent_type(&self) -> AgentType;

    /// Tool names this worker is authorized to use.
    fn allowed_tools(&self) -> Vec<String>;

    /// Human-readable name/description of this worker.
    fn description(&self) -> &str;
}
