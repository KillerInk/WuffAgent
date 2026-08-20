use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for an agent instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(id: &str) -> Self { Self(id.to_string()) }
    pub fn generate() -> Self {
        Self(format!("agent-{}", Uuid::new_v4()))
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The type/category of a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[derive(Default)]
pub enum AgentType {
    Research,
    Coding,
    Implementation,
    #[default]
    General,
}

impl<'de> Deserialize<'de> for AgentType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "research" => Ok(AgentType::Research),
            "coding" => Ok(AgentType::Coding),
            "implementation" => Ok(AgentType::Implementation),
            "general" => Ok(AgentType::General),
            _ => Err(serde::de::Error::custom(format!(
                "unknown agent type: {}",
                s
            ))),
        }
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentType::Research => write!(f, "research"),
            AgentType::Coding => write!(f, "coding"),
            AgentType::Implementation => write!(f, "implementation"),
            AgentType::General => write!(f, "general"),
        }
    }
}

/// Task execution status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Retryable,
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
            TaskStatus::Retryable => write!(f, "retryable"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// The result returned by an Agent after executing a request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentResult {
    pub task_id: String,
    pub agent_id: String,
    pub agent_type: AgentType,
    pub status: TaskStatus,
    /// The output payload (success data, error message, or partial result).
    pub output: serde_json::Value,
    /// Human-readable summary for logging.
    #[serde(default)]
    pub summary: String,
    /// Timing information.
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Metadata about an agent for discovery.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Unique name of the agent.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// The agent type.
    pub agent_type: AgentType,
    /// Tool names this agent is authorized to use.
    pub allowed_tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_generate() {
        let id1 = AgentId::generate();
        let id2 = AgentId::generate();
        assert_ne!(id1, id2);
        assert!(id1.0.starts_with("agent-"));
    }
}
