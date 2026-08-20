use super::types::{AgentMetadata, AgentResult};

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
    ToolError(#[from] crate::tools::types::ToolError),
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

/// Trait for agents that can be invoked by other agents.
#[async_trait::async_trait]
pub trait AgentInvocation: Send + Sync {
    /// Invoke this agent with a task description and return the result.
    async fn invoke(
        &self,
        request: &str,
        context: &serde_json::Value,
    ) -> AgentResultType<AgentResult>;

    /// Get metadata about this agent for discovery.
    fn metadata(&self) -> AgentMetadata;
}
