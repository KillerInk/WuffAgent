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
    #[error("Internal error: {0}")]
    Internal(String),
}
