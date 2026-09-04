use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tokio_util::sync::CancellationToken;
use tracing;

use super::traits::{AgentError, AgentInvocation};
use super::types::{AgentResult, AgentType};

/// Registry of agents that can be invoked by other agents.
///
/// This enables inter-agent communication where one agent can invoke
/// another agent as a sub-task.
pub struct AgentInvocationRegistry {
    agents: RwLock<HashMap<String, Arc<dyn AgentInvocation>>>,
}

impl Clone for AgentInvocationRegistry {
    fn clone(&self) -> Self {
        Self {
            agents: RwLock::new(self.agents.read().unwrap().clone()),
        }
    }
}

impl AgentInvocationRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
        }
    }

    /// Register an invokable agent.
    pub fn register(&self, name: &str, agent: Arc<dyn AgentInvocation>) {
        self.agents
            .write()
            .unwrap()
            .insert(name.to_string(), agent);
        tracing::info!("Registered invokable agent: {}", name);
    }

    /// Get an agent by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentInvocation>> {
        self.agents.read().unwrap().get(name).cloned()
    }

    /// Check if an agent is registered.
    pub fn has(&self, name: &str) -> bool {
        self.agents.read().unwrap().contains_key(name)
    }

    /// Find agents by type.
    pub fn find_by_type(&self, agent_type: &AgentType) -> Vec<String> {
        self.agents
            .read()
            .unwrap()
            .iter()
            .filter(|(_, agent)| &agent.metadata().agent_type == agent_type)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get all registered agent names.
    pub fn names(&self) -> Vec<String> {
        self.agents.read().unwrap().keys().cloned().collect()
    }

    /// Invoke an agent by name.
    pub async fn invoke(
        &self,
        target: &str,
        request: &str,
        context: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<AgentResult, AgentError> {
        let agent = self
            .get(target)
            .ok_or_else(|| AgentError::AgentNotFound(format!("Agent '{}' not found", target)))?;
        agent.invoke(request, context, cancel_token).await
    }
}

impl Default for AgentInvocationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::traits::{AgentError, AgentInvocation};
    use super::super::types::{AgentMetadata, AgentResult, AgentType, TaskStatus};

    #[tokio::test]
    async fn test_register_and_get() {
        let registry = AgentInvocationRegistry::new();
        let mock = Arc::new(MockAgent::new("test-agent"));
        registry.register("test-agent", mock);
        assert!(registry.has("test-agent"));
        assert!(registry.get("test-agent").is_some());
        assert!(registry.get("missing").is_none());
    }

    #[tokio::test]
    async fn test_find_by_type() {
        let registry = AgentInvocationRegistry::new();
        registry.register(
            "researcher",
            Arc::new(MockAgent::new_with_type("researcher", AgentType::Research)),
        );
        registry.register(
            "coder",
            Arc::new(MockAgent::new_with_type("coder", AgentType::Coding)),
        );
        let researchers = registry.find_by_type(&AgentType::Research);
        assert_eq!(researchers, vec!["researcher"]);
    }

    #[tokio::test]
    async fn test_invoke() {
        let registry = AgentInvocationRegistry::new();
        let mock = Arc::new(MockAgent::new("test-agent"));
        registry.register("test-agent", mock);
        let token = CancellationToken::new();
        let result = registry.invoke("test-agent", "test request", &serde_json::json!({}), &token).await;
        assert!(result.is_ok());
    }

    struct MockAgent {
        name: String,
        agent_type: AgentType,
    }

    impl MockAgent {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                agent_type: AgentType::General,
            }
        }
        fn new_with_type(name: &str, agent_type: AgentType) -> Self {
            Self {
                name: name.to_string(),
                agent_type,
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentInvocation for MockAgent {
        async fn invoke(
            &self,
            _request: &str,
            _context: &serde_json::Value,
            _cancel_token: &CancellationToken,
        ) -> Result<AgentResult, AgentError> {
            Ok(AgentResult {
                task_id: "test".to_string(),
                agent_id: self.name.clone(),
                agent_type: self.agent_type.clone(),
                status: TaskStatus::Completed,
                output: serde_json::json!({ "result": "mock" }),
                summary: "mock result".to_string(),
                duration_ms: 0,
                completed_at: None,
            })
        }
        fn metadata(&self) -> AgentMetadata {
            AgentMetadata {
                name: self.name.clone(),
                description: format!("Mock agent: {}", self.name),
                agent_type: self.agent_type.clone(),
                allowed_tools: Vec::new(),
            }
        }
    }
}
