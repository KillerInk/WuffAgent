use std::cmp::min;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tracing;

use super::traits::{AgentError, WorkerAgent};
use super::worker::GenericWorker;
use super::workers::generic::ExecutingWorker;
use crate::agents::config::WorkerConfig;
use crate::tools::registry::ToolRegistry;
use crate::tools::lib::TracingToolLogger;

/// Registry that maps worker names to factory functions.
type WorkerFactory = Arc<dyn Fn() -> Box<dyn WorkerAgent> + Send + Sync>;

pub struct WorkerRegistry {
    workers: RwLock<HashMap<String, WorkerFactory>>,
    /// Shared tool registry with builtins — all workers use this.
    tool_registry: Arc<ToolRegistry>,
    /// Registry for inter-agent invocation.
    invocation_registry: Arc<super::invocation_registry::AgentInvocationRegistry>,
}

impl WorkerRegistry {
    pub fn new() -> Self {
        let tool_registry = Arc::new(ToolRegistry::new(
            vec![],
            Arc::new(TracingToolLogger),
        ));
        let invocation_registry = Arc::new(super::invocation_registry::AgentInvocationRegistry::new());
        // Register built-in tools so workers can actually use them
        crate::tools::builtin::register_builtins(&tool_registry, &invocation_registry)
            .expect("failed to register builtin tools");
        let mut registry = Self {
            workers: RwLock::new(HashMap::new()),
            tool_registry,
            invocation_registry,
        };
        registry.register_builtins();
        registry
    }

    /// Get a clone of the shared tool registry for use by workers.
    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        self.tool_registry.clone()
    }

    /// Get the invocation registry for inter-agent calls.
    pub fn invocation_registry(&self) -> Arc<super::invocation_registry::AgentInvocationRegistry> {
        self.invocation_registry.clone()
    }

    /// Register a worker factory by name.
    pub fn register(
        &self,
        name: &str,
        factory: impl Fn() -> Box<dyn WorkerAgent> + Send + Sync + 'static,
    ) {
        let factory = Arc::new(factory);
        self.workers
            .write()
            .unwrap()
            .insert(name.to_string(), factory.clone());
        // Also register as invokable by other agents
        let name_clone = name.to_string();
        let inv_reg = self.invocation_registry.clone();
        inv_reg.register(&name_clone, Arc::new(super::invocation_registry::InvokableWorker::new(
            &name_clone,
            factory,
        )));
        tracing::info!("Registered worker: {}", name);
    }

    /// Spawn a worker by name.
    pub fn spawn(&self, name: &str) -> Option<Box<dyn WorkerAgent>> {
        self.workers
            .read()
            .unwrap()
            .get(name)
            .map(|f| f())
    }

    /// Check if a worker is registered.
    pub fn has(&self, name: &str) -> bool {
        self.workers.read().unwrap().contains_key(name)
    }

    /// Get all registered worker names.
    pub fn names(&self) -> Vec<String> {
        self.workers.read().unwrap().keys().cloned().collect()
    }

    /// Load workers from config and register them (skips disabled agents).
    pub async fn load_from_configs(
        &self,
        configs: Vec<WorkerConfig>,
    ) -> Result<usize, AgentError> {
        let mut count = 0;
        for config in configs {
            if !config.enabled {
                tracing::info!("Skipping disabled agent: {}", config.name);
                continue;
            }
            let name = config.name.clone();
            let desc = config.description.clone();
            let system_prompt = config.system_prompt.clone();
            let tools = config.allowed_tools.clone();
            let name_closure = name.clone();
            let desc_closure = desc.clone();
            let system_prompt_closure = system_prompt.clone();
            let tools_closure = tools.clone();
            let tool_mgr = self.tool_registry.clone();

            self.register(&name, move || {
                let tm = Arc::new(tokio::sync::Mutex::new(
                    crate::tools::ToolManager::new(tool_mgr.clone()),
                ));
                Box::new(GenericWorker::new(&name_closure, &desc_closure, tools_closure.clone(), &system_prompt_closure, tm))
            });
            count += 1;
        }
        tracing::info!("Loaded {} worker(s) from config", count);
        Ok(count)
    }

    /// Remove all non-builtin registrations and reload from disk.
    /// Returns the number of workers re-registered.
    pub async fn reload(
        &self,
        workers_dir: &std::path::Path,
    ) -> Result<usize, AgentError> {
        // Remove all registered names except builtins
        let builtin_names = ["default".to_string(), "executing".to_string()];
        let all_names = self.names();
        for name in all_names {
            if !builtin_names.contains(&name) {
                self.remove(&name);
            }
        }

        // Reload from disk
        let configs = WorkerConfig::load_all_from_dir(workers_dir)?;
        let count = self.load_from_configs(configs).await?;
        tracing::info!("Reloaded {} worker(s) from {:?}", count, workers_dir);
        Ok(count)
    }

    /// Remove a worker by name.
    pub fn remove(&self, name: &str) -> bool {
        self.workers.write().unwrap().remove(name).is_some()
    }

    /// Find the best worker for a task based on tool requirements.
    /// Returns the worker name and its config info.
    pub async fn find_best_worker(
        &self,
        task: &super::types::Task,
        registered_workers: &[WorkerConfig],
    ) -> Option<(String, WorkerConfig)> {
        let task_tool_names: Vec<&str> = task
            .input
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let task_description = task.description.to_lowercase();
        let _task_agent_type = &task.agent_type;

        let mut candidates: Vec<(String, WorkerConfig, i64)> = Vec::new();

        for config in registered_workers {
            let mut score = 0i64;

            // Prefer workers whose allowed tools match the task's required tools
            for required_tool in &task_tool_names {
                if config.allowed_tools.iter().any(|t| t == *required_tool) {
                    score += 10;
                }
            }

            // Prefer workers whose description matches the task description
            let config_desc = config.description.to_lowercase();
            if task_description.contains(&config_desc[..min(20, config_desc.len())]) {
                score += 5;
            }

            // Prefer workers with lower priority value
            score -= config.priority as i64 * 2;

            candidates.push((config.name.clone(), config.clone(), score));
        }

        // Also consider built-in worker types
        let worker_names = self.names();
        for wname in &worker_names {
            let tool_overlap = if task_tool_names.is_empty() {
                5
            } else {
                let worker_tools = registered_workers
                    .iter()
                    .find(|c| &c.name == wname)
                    .map(|c| c.allowed_tools.clone())
                    .unwrap_or_default();
                task_tool_names
                    .iter()
                    .filter(|t| worker_tools.iter().any(|wt| wt == **t))
                    .count()
            };

            if tool_overlap > 0 {
                let existing_score = candidates.iter().position(|(n, _, _)| n == wname);
                match existing_score {
                    Some(idx) => {
                        let (_, _, ref mut score) = candidates[idx];
                        *score += tool_overlap as i64 * 10;
                    }
                    None => {
                        let config = registered_workers
                            .iter()
                            .find(|c| &c.name == wname)
                            .cloned();
                        match config {
                            Some(c) => {
                                candidates.push((wname.clone(), c, tool_overlap as i64 * 10));
                            }
                            None => {
                                candidates.push((wname.clone(), WorkerConfig::default(), tool_overlap as i64 * 10));
                            }
                        }
                    }
                }
            }
        }

        if candidates.is_empty() {
            // Fall back to any available worker
            if let Some(first_name) = worker_names.first() {
                let fallback_config = WorkerConfig {
                    name: first_name.clone(),
                    description: String::from("Fallback worker"),
                    system_prompt: String::from("You are a fallback worker."),
                    allowed_tools: vec!["file_io".to_string()],
                    priority: 999,
                    max_concurrent: 1,
                    enabled: true,
                    can_invoke: vec![],
                    handoff_enabled: false,
                };
                return Some((first_name.clone(), fallback_config));
            }
            return None;
        }

        // Sort by score descending
        candidates.sort_by_key(|b| std::cmp::Reverse(b.2));

        // Return the best candidate
        let (best_name, best_config, _) = &candidates[0];
        Some((best_name.clone(), best_config.clone()))
    }

    fn register_builtins(&mut self) {
        let tool_mgr = Arc::new(tokio::sync::Mutex::new(
            crate::tools::ToolManager::new(self.tool_registry.clone()),
        ));
        let tool_mgr_default = tool_mgr.clone();
        self.register("default", move || {
            Box::new(GenericWorker::new(
                "default",
                "Default fallback worker",
                vec!["file_io".to_string()],
                "You are a default worker.",
                tool_mgr_default.clone(),
            ))
        });
        self.register("executing", move || {
            Box::new(ExecutingWorker::new(
                "executing",
                "Executing worker with full tool access",
                super::types::AgentType::General,
                vec![
                    "file_io".to_string(),
                    "web_search".to_string(),
                    "calculation".to_string(),
                ],
                "You are an executing worker.",
                tool_mgr.clone(),
            ))
        });
    }
}

impl Default for WorkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::AgentType;

    #[test]
    fn test_register_and_spawn() {
        let registry = WorkerRegistry::new();
        let tool_mgr = Arc::new(tokio::sync::Mutex::new(
            crate::tools::ToolManager::new(registry.tool_registry().clone()),
        ));
        registry
            .register("test", move || Box::new(GenericWorker::new("test", "Test", vec![], "You are a test worker.", tool_mgr.clone())));

        assert!(registry.has("test"));
        assert!(!registry.has("nonexistent"));

        let worker = registry.spawn("test");
        assert!(worker.is_some());
    }

    #[tokio::test]
    async fn test_builtin_default_spawnable() {
        // Verify builtins are registered and spawnable immediately after construction
        let registry = WorkerRegistry::new();
        assert!(registry.has("default"), "builtin 'default' must be registered");
        assert!(registry.has("executing"), "builtin 'executing' must be registered");

        let worker = registry.spawn("default");
        assert!(worker.is_some(), "spawn('default') must return Some");
        let worker = worker.unwrap();
        assert_eq!(worker.agent_type(), AgentType::General);
    }

    #[tokio::test]
    async fn test_spawn_after_load_from_configs() {
        // Simulate runtime: new registry -> load configs -> spawn default
        let registry = WorkerRegistry::new();
        let configs = vec![
            WorkerConfig {
                name: "research".to_string(),
                description: "Research worker".to_string(),
                system_prompt: "You are a researcher.".to_string(),
                allowed_tools: vec!["web_search".to_string()],
                priority: 0,
                max_concurrent: 1,
                enabled: true,
                can_invoke: vec![],
                handoff_enabled: false,
            },
        ];
        registry.load_from_configs(configs).await.unwrap();

        assert!(registry.has("default"), "builtin must survive load_from_configs");
        assert!(registry.has("research"), "loaded config must be registered");
        assert!(registry.has("executing"), "builtin must survive load_from_configs");

        let worker = registry.spawn("default");
        assert!(worker.is_some(), "spawn('default') must still work after config load");
    }

    #[tokio::test]
    async fn test_spawn_default_with_arc_clone() {
        // Simulate runtime: Arc<WorkerRegistry> clone then spawn
        use std::sync::Arc;
        let registry = Arc::new(WorkerRegistry::new());

        assert!(registry.has("default"));
        let worker = registry.spawn("default");
        assert!(worker.is_some(), "spawn('default') via Arc must work");

        // Clone the Arc (like supervisor does)
        let cloned = registry.clone();
        let worker2 = cloned.spawn("default");
        assert!(worker2.is_some(), "spawn('default') via cloned Arc must work");
    }

    #[tokio::test]
    async fn test_find_best_worker() {
        let registry = WorkerRegistry::new();
        let configs = vec![
            WorkerConfig {
                name: "research".to_string(),
                description: "Research worker".to_string(),
                system_prompt: "You are a researcher.".to_string(),
                allowed_tools: vec!["web_search".to_string()],
                priority: 0,
                max_concurrent: 1,
                enabled: true,
                can_invoke: vec![],
                handoff_enabled: false,
            },
            WorkerConfig {
                name: "coding".to_string(),
                description: "Coding worker".to_string(),
                system_prompt: "You are a coder.".to_string(),
                allowed_tools: vec!["file_io".to_string()],
                priority: 1,
                max_concurrent: 2,
                enabled: true,
                can_invoke: vec![],
                handoff_enabled: false,
            },
        ];

        registry.load_from_configs(configs).await.unwrap();

        let task = super::super::types::Task {
            id: "test-1".to_string(),
            description: "Search the web for information".to_string(),
            agent_type: super::super::types::AgentType::Research,
            input: serde_json::json!({
                "tools": ["web_search"],
                "query": "test"
            }),
            depends_on: None,
            max_retries: 3,
            priority: 0,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };

        let result = registry.find_best_worker(&task, &Vec::new()).await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_find_best_worker_empty_configs_falls_back_to_builtin() {
        // When no worker configs are loaded, find_best_worker should fall back to builtins
        let registry = WorkerRegistry::new();
        let empty: Vec<WorkerConfig> = Vec::new();

        let task = super::super::types::Task {
            id: "test-2".to_string(),
            description: "List files in a directory".to_string(),
            agent_type: super::super::types::AgentType::Research,
            input: serde_json::json!({"tool": "list_directory", "arguments": {"path": "."}}),
            depends_on: None,
            max_retries: 3,
            priority: 0,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };

        let result = registry.find_best_worker(&task, &empty).await;
        // Should find 'default' as fallback since it's the first builtin
        assert!(result.is_some(), "find_best_worker must find fallback when configs are empty");
        let (name, _) = result.unwrap();
        // 'executing' scores higher than 'default' when tool overlap is considered (5 built-in tools vs 1)
        assert!(name == "executing" || name == "default", "must fall back to a builtin, got {}", name);
    }

    #[tokio::test]
    async fn test_supervisor_spawn_best_worker_flow() {
        // End-to-end test mimicking the supervisor's spawn_best_worker logic
        use super::super::types::AgentType;

        let registry = Arc::new(WorkerRegistry::new());
        let configs: Vec<WorkerConfig> = Vec::new(); // no loaded configs

        let task = super::super::types::Task {
            id: "test-3".to_string(),
            description: "Check for precision errors in SYCL code".to_string(),
            agent_type: AgentType::Research,
            input: serde_json::json!({"tool": "list_directory", "arguments": {"path": "."}}),
            depends_on: None,
            max_retries: 3,
            priority: 0,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };

        // This mirrors the supervisor's spawn_best_worker logic
        let worker = if let Some((name, _config)) = registry.find_best_worker(&task, &configs).await {
            registry.spawn(&name)
        } else {
            registry.spawn("default")
        };

        assert!(worker.is_some(), "supervisor must be able to spawn a worker");
    }
}
