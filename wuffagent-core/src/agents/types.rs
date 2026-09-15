use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A pending request to hand the session over to another agent profile.
///
/// Written by the `handoff` tool into the per-execution mailbox and consumed
/// by `Agent::execute`, which switches to the target agent on the same
/// conversation store.
#[derive(Clone, Debug)]
pub struct HandoffRequest {
    /// Target agent name.
    pub agent: String,
    /// Resolved target profile (system prompt, tools, shell, effort, …).
    pub config: super::config::AgentConfig,
    /// Handoff instructions for the target agent (used as its task / memory
    /// query and shown in the UI banner).
    pub task: String,
}

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
