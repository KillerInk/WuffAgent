use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;
use tracing;

use super::agent::Agent;
use super::config::{AgentConfig, RecoveryPolicy};
use super::invocation_registry::AgentInvocationRegistry;
use super::llm_client::LlmClient;
use super::traits::AgentError;
use crate::client::ChatClient;
use crate::tools::registry::ToolRegistry;
use crate::tools::ToolManager;
use crate::types::AppEvent;

/// Registry of all available agents, loaded from JSON config files.
/// Provides routing prompt caching for efficient agent selection.
pub struct AgentRegistry {
    agents: HashMap<String, AgentConfig>,
    routing_prompt: String,
    prompt_dirty: bool,
}

impl AgentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            routing_prompt: String::new(),
            prompt_dirty: true,
        }
    }

    /// Load agents from the given directories.
    /// Scans both `agents/` and legacy `workers/` directories.
    /// First-seen name wins for deduplication.
    pub fn load(search_dirs: Vec<PathBuf>, global_registry: &ToolRegistry) -> Result<Self, AgentError> {
        let mut agents = HashMap::new();

        for dir in search_dirs {
            if !dir.exists() {
                continue;
            }

            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Failed to read directory {:?}: {}", dir, e);
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().map(|e| e == "json").unwrap_or(false) {
                    match Self::load_agent_config(&path, global_registry) {
                        Ok(config) => {
                            if agents.insert(config.name.clone(), config).is_some() {
                                tracing::warn!(
                                    "Agent '{}' already loaded, skipping duplicate from {:?}",
                                    path.file_name().unwrap_or_default().to_string_lossy(),
                                    dir
                                );
                            } else {
                                tracing::info!("Loaded agent: {} from {:?}", path.file_name().unwrap_or_default().to_string_lossy(), dir);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        let mut registry = Self {
            agents,
            routing_prompt: String::new(),
            prompt_dirty: true,
        };
        registry.build_routing_prompt_internal();
        Ok(registry)
    }

    /// Load a single agent config from a JSON file.
    /// Tries new AgentConfig format first, falls back to legacy WorkerConfig.
    pub fn load_agent_config(path: &Path, global_registry: &ToolRegistry) -> Result<AgentConfig, AgentError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| AgentError::ConfigError(format!(
                "Failed to read config from {:?}: {}", path, e
            )))?;

        // Try new AgentConfig format first
        if let Ok(config) = serde_json::from_str::<AgentConfig>(&content) {
            Self::validate_config(global_registry, &config)?;
            return Ok(config);
        }

        // Fallback: legacy WorkerConfig format — migrate to AgentConfig
        let legacy: super::config::WorkerConfig = serde_json::from_str(&content)
            .map_err(|e| AgentError::ConfigError(format!(
                "Failed to parse config from {:?} as either AgentConfig or WorkerConfig: {}", path, e
            )))?;

        tracing::info!(
            "Migrating legacy worker config '{}' from {:?} to AgentConfig",
            legacy.name,
            path.file_name().unwrap_or_default()
        );

        let config = AgentConfig {
            name: legacy.name,
            description: legacy.description,
            system_prompt: legacy.system_prompt,
            allowed_tools: legacy.allowed_tools,
            enabled: legacy.enabled,
            max_depth: 5,
            recovery_policy: RecoveryPolicy::default(),
            max_plan_iterations: 5,
            max_parallel_workers: 4,
            task_timeout_ms: 60_000,
            auto_refine: true,
            workers_dir: PathBuf::from(""),
            custom_prompts: HashMap::new(),
            reasoning_effort: legacy.reasoning_effort,
        };

        Self::validate_config(global_registry, &config)?;
        Ok(config)
    }

    /// Get an agent config by name.
    pub fn get_agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.get(name)
    }

    /// Returns the number of agents in the registry.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Insert an agent (test helper).
    #[cfg(test)]
    pub fn add_agent_for_test(&mut self, config: AgentConfig) {
        self.agents.insert(config.name.clone(), config);
        self.prompt_dirty = true;
    }

    /// Returns all agents in the registry (for internal use).
    pub(crate) fn get_all_agents(&self) -> Vec<&AgentConfig> {
        self.agents.values().collect()
    }

    /// Returns the cached routing prompt (read-only access).
    pub(crate) fn routing_prompt(&self) -> &str {
        &self.routing_prompt
    }

    /// Get a list of all available agent names as a comma-separated string.
    pub fn available_agent_names(&self) -> String {
        self.agents.keys()
            .filter(|k| self.agents.get(*k).map(|a| a.enabled).unwrap_or(false))
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Write an agent config to a JSON file in the given directory.
    pub fn write_agent_config(&self, config: &AgentConfig, dir: &Path) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(dir)
            .map_err(|e| AgentError::ConfigError(format!("Failed to create agents dir {:?}: {}", dir, e)))?;
        let path = dir.join(format!("{}.json", config.name));
        let content = serde_json::to_string_pretty(config)
            .map_err(|e| AgentError::ConfigError(format!("Failed to serialize agent config: {}", e)))?;
        std::fs::write(&path, content)
            .map_err(|e| AgentError::ConfigError(format!("Failed to write agent config to {:?}: {}", path, e)))?;
        tracing::info!("Wrote agent config to {:?}", path);
        Ok(path)
    }

    /// Find a fallback agent when the LLM uses a wrong name.
    /// Tries exact match first, then substring match, then falls back to "general" or first enabled agent.
    pub(crate) fn find_fallback_agent(&self, requested: &str) -> String {
        if self.agents.contains_key(requested) {
            return requested.to_string();
        }
        let lower = requested.to_lowercase();
        for name in self.agents.keys() {
            if name.to_lowercase() == lower {
                return name.clone();
            }
        }
        for name in self.agents.keys() {
            let name_lower = name.to_lowercase();
            if name_lower.contains(&lower) || lower.contains(&name_lower) {
                return name.clone();
            }
        }
        if self.agents.contains_key("general") {
            return "general".to_string();
        }
        self.agents.keys()
            .filter(|k| self.agents.get(*k).map(|a| a.enabled).unwrap_or(false))
            .next()
            .map(|k| k.clone())
            .unwrap_or_else(|| "general".to_string())
    }

    /// Build the routing prompt that describes all agents for LLM selection.
    /// Caches the result; use `invalidate_routing_prompt()` after mutations.
    pub fn build_routing_prompt(&mut self) -> &str {
        if self.prompt_dirty {
            self.routing_prompt = self.build_routing_prompt_internal();
            self.prompt_dirty = false;
        }
        &self.routing_prompt
    }

    /// Invalidate the cached routing prompt.
    pub fn invalidate_routing_prompt(&mut self) {
        self.prompt_dirty = true;
    }

    /// Internal build — constructs the routing prompt from all enabled agents.
    fn build_routing_prompt_internal(&mut self) -> String {
        let mut prompt = String::from("You are a multi-agent system router. Choose the best agent for each task.\n\n");
        prompt.push_str("Available agents (you MUST use only these names):\n");

        let mut agents: Vec<_> = self.agents.values().filter(|a| a.enabled).collect();
        agents.sort_by_key(|a| a.name.clone());

        for agent in agents {
            let tools = if agent.allowed_tools.is_empty() {
                "none".to_string()
            } else {
                agent.allowed_tools.join(", ")
            };
            prompt.push_str(&format!(
                "- **{}**: {} [tools: {}]\n",
                agent.name, agent.description, tools
            ));
        }

        prompt.push_str("\nIf the task requires a specific agent, respond with a single JSON object: {\"agent\": \"<agent_name>\", \"task\": \"<delegated_task>\"}\n");
        prompt.push_str("If the task can be answered directly, respond with plain text.\n");
        prompt.push_str("IMPORTANT: Use ONLY the exact agent names listed above. Do NOT invent new agent names. These will be rejected.\n");
        prompt
    }

    /// Validate that all tools referenced by an agent config exist in the global registry.
    pub fn validate_config(
        global_registry: &ToolRegistry,
        config: &AgentConfig,
    ) -> Result<(), AgentError> {
        for tool_name in &config.allowed_tools {
            if global_registry.get(tool_name).is_none() {
                tracing::warn!(
                    "Agent '{}' references unknown tool: {}",
                    config.name,
                    tool_name
                );
            }
        }
        Ok(())
    }

    /// Build an `Agent` instance from this registry, given an LLM client.
    pub fn build_agent(
        &self,
        name: &str,
        llm_client: Arc<dyn LlmClient>,
        event_tx: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
        client: Arc<ChatClient>,
        invocation_registry: Arc<AgentInvocationRegistry>,
        memory: Option<Arc<crate::memory::MemoryManager>>,
    ) -> Option<Agent> {
        let config = self.agents.get(name)?.clone();
        let tool_manager = Arc::new(Mutex::new(crate::tools::ToolManager::new_empty()));
        Some(Agent::new(
            config,
            llm_client,
            tool_manager,
            invocation_registry,
            event_tx,
            client,
            memory,
        ))
    }

    /// Build an invocation registry that references all enabled agents in this registry,
    /// wiring each entry to actually run the sub-agent via the provided LLM client,
    /// tool manager, and client.
    pub fn build_invocation_registry(
        &self,
        llm_client: Arc<dyn LlmClient>,
        tool_manager: Arc<Mutex<ToolManager>>,
        client: Arc<ChatClient>,
    ) -> Arc<AgentInvocationRegistry> {
        let registry = Arc::new(AgentInvocationRegistry::new());
        for (name, config) in &self.agents {
            if !config.enabled {
                continue;
            }
            let name_str = name.clone();
            let config_clone = config.clone();
            let llm_client_clone = llm_client.clone();
            let tool_manager_clone = tool_manager.clone();
            let client_clone = client.clone();
            let inv_reg = registry.clone();
            let agent = Arc::new(RegistryAgentInvocation {
                name: name_str.clone(),
                config: config_clone,
                llm_client: llm_client_clone,
                tool_manager: tool_manager_clone,
                client: client_clone,
                invocation_registry: inv_reg,
            });
            registry.register(&name_str, agent);
        }
        registry
    }
}

/// A wrapper that adapts an AgentConfig into an AgentInvocation.
/// This allows the registry to invoke agents by name by actually running
/// the sub-agent's LLM tool loop.
struct RegistryAgentInvocation {
    name: String,
    config: AgentConfig,
    llm_client: Arc<dyn LlmClient>,
    tool_manager: Arc<Mutex<ToolManager>>,
    client: Arc<ChatClient>,
    invocation_registry: Arc<AgentInvocationRegistry>,
}

#[async_trait::async_trait]
impl super::traits::AgentInvocation for RegistryAgentInvocation {
    async fn invoke(
        &self,
        request: &str,
        _context: &serde_json::Value,
    ) -> super::traits::AgentResultType<super::types::AgentResult> {
        let config = self.config.clone();
        let llm_client = self.llm_client.clone();
        let tool_manager = self.tool_manager.clone();
        let client = self.client.clone();
        let invocation_registry = self.invocation_registry.clone();

        // Run the sub-agent's LLM loop synchronously within the current runtime
        // (or create a temporary one if outside a runtime).
        let task_id = format!("inv-{}", uuid::Uuid::new_v4());
        let request = request.to_string();
        let start = std::time::Instant::now();

        let result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let join = handle.spawn(async move {
                let agent = Agent::new(
                    config,
                    llm_client,
                    tool_manager,
                    invocation_registry,
                    None, // no event tx — sub-agent output is captured in the tool result
                    client,
                    None, // sub-agents don't have memory access
                );
                agent.execute(&request, &CancellationToken::new()).await
            });
            handle.block_on(join)
                .map_err(|e| AgentError::Internal(format!("Sub-agent join error: {}", e)))?
        } else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| AgentError::Internal(format!("Failed to create runtime: {}", e)))?;
            rt.block_on(async move {
                let agent = Agent::new(
                    config,
                    llm_client,
                    tool_manager,
                    invocation_registry,
                    None,
                    client,
                    None, // sub-agents don't have memory access
                );
                agent.execute(&request, &CancellationToken::new()).await
            })
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(response) => Ok(super::types::AgentResult {
                task_id,
                agent_id: self.name.clone(),
                agent_type: super::types::AgentType::General,
                status: super::types::TaskStatus::Completed,
                output: serde_json::json!({ "result": response }),
                summary: response.chars().take(500).collect(),
                duration_ms,
                completed_at: None,
            }),
            Err(e) => Ok(super::types::AgentResult {
                task_id,
                agent_id: self.name.clone(),
                agent_type: super::types::AgentType::General,
                status: super::types::TaskStatus::Failed,
                output: serde_json::json!({ "error": e }),
                summary: format!("Sub-agent failed: {}", e),
                duration_ms,
                completed_at: None,
            }),
        }
    }

    fn metadata(&self) -> super::types::AgentMetadata {
        super::types::AgentMetadata {
            name: self.name.clone(),
            description: self.config.description.clone(),
            agent_type: super::types::AgentType::General,
            allowed_tools: self.config.allowed_tools.clone(),
        }
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Error type for agent registry operations.
/// Re-exported from traits for convenience.
pub use super::traits::AgentError as RegistryError;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::tools::builtin;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::types::TracingToolLogger;

    fn make_test_registry() -> ToolRegistry {
        let logger = Arc::new(TracingToolLogger);
        let registry = ToolRegistry::new(vec![], logger);
        let invocation_registry = AgentInvocationRegistry::new();
        builtin::register_builtins(&registry, &invocation_registry).expect("failed to register builtins");
        registry
    }

    #[test]
    fn test_registry_new_is_empty() {
        let registry = AgentRegistry::new();
        assert_eq!(registry.agent_count(), 0);
    }

    #[test]
    fn test_load_agent_config_new_format() {
        let tool_registry = make_test_registry();
        let dir = std::env::temp_dir().join("wuffagent_test_registry_new");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config = AgentConfig {
            name: "test_agent".to_string(),
            description: "A test agent".to_string(),
            system_prompt: "Be helpful.".to_string(),
            allowed_tools: vec!["file_io".to_string()],
            enabled: true,
            max_depth: 3,
            recovery_policy: RecoveryPolicy::Retry,
            max_plan_iterations: 3,
            max_parallel_workers: 2,
            task_timeout_ms: 30_000,
            auto_refine: false,
            workers_dir: dir.clone(),
            ..Default::default()
        };
        let path = dir.join("test_agent.json");
        let content = serde_json::to_string_pretty(&config).unwrap();
        std::fs::write(&path, content).unwrap();

        let loaded = AgentRegistry::load_agent_config(&path, &tool_registry).unwrap();
        assert_eq!(loaded.name, "test_agent");
        assert_eq!(loaded.max_depth, 3);
        assert_eq!(loaded.recovery_policy, RecoveryPolicy::Retry);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_agent_config_legacy_fallback() {
        let tool_registry = make_test_registry();
        let dir = std::env::temp_dir().join("wuffagent_test_registry_legacy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let legacy_json = r#"{
            "name": "legacy_agent",
            "description": "A legacy worker",
            "personality": "You are legacy.",
            "allowed_tools": ["calculation"],
            "priority": 0,
            "max_concurrent": 1,
            "enabled": true
        }"#;
        let path = dir.join("legacy_agent.json");
        std::fs::write(&path, legacy_json).unwrap();

        let loaded = AgentRegistry::load_agent_config(&path, &tool_registry).unwrap();
        assert_eq!(loaded.name, "legacy_agent");
        assert_eq!(loaded.system_prompt, "You are legacy.");
        assert_eq!(loaded.max_depth, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_from_directory() {
        let tool_registry = make_test_registry();
        let dir = std::env::temp_dir().join("wuffagent_test_registry_load");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config1 = AgentConfig {
            name: "agent_a".to_string(),
            description: "Agent A".to_string(),
            allowed_tools: vec![],
            ..Default::default()
        };
        let config2 = AgentConfig {
            name: "agent_b".to_string(),
            description: "Agent B".to_string(),
            allowed_tools: vec!["file_io".to_string()],
            enabled: false,
            ..Default::default()
        };
        std::fs::write(
            dir.join("agent_a.json"),
            serde_json::to_string_pretty(&config1).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("agent_b.json"),
            serde_json::to_string_pretty(&config2).unwrap(),
        )
        .unwrap();

        let mut registry = AgentRegistry::load(vec![dir.clone()], &tool_registry).unwrap();
        assert_eq!(registry.agent_count(), 2);
        assert!(registry.get_agent("agent_a").is_some());
        assert!(registry.get_agent("agent_b").is_some());
        assert!(registry.get_agent("nonexistent").is_none());

        let prompt = registry.build_routing_prompt();
        assert!(prompt.contains("agent_a"));
        assert!(!prompt.contains("agent_b"));

        drop(registry);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_routing_prompt_invalidation() {
        let tool_registry = make_test_registry();
        let dir = std::env::temp_dir().join("wuffagent_test_registry_invalidate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config = AgentConfig {
            name: "dynamic_agent".to_string(),
            description: "Dynamic".to_string(),
            enabled: true,
            ..Default::default()
        };
        std::fs::write(
            dir.join("dynamic_agent.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let mut registry = AgentRegistry::load(vec![dir.clone()], &tool_registry).unwrap();
        let prompt1 = registry.build_routing_prompt();
        assert!(prompt1.contains("dynamic_agent"));

        registry.invalidate_routing_prompt();
        let config2 = AgentConfig {
            name: "another_agent".to_string(),
            description: "Another".to_string(),
            enabled: true,
            ..Default::default()
        };
        registry.agents.insert("another_agent".to_string(), config2);
        let prompt2 = registry.build_routing_prompt();
        assert!(prompt2.contains("another_agent"));
        assert!(prompt2.contains("dynamic_agent"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
