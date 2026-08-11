use std::cmp::min;
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing;

use super::traits::{AgentError, WorkerAgent};
use super::worker::GenericWorker;
use super::workers::generic::ExecutingWorker;
use crate::agents::config::WorkerConfig;

/// Registry that maps worker names to factory functions.
pub struct WorkerRegistry {
    workers: RwLock<HashMap<String, Arc<dyn Fn() -> Box<dyn WorkerAgent> + Send + Sync>>>,
}

impl WorkerRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            workers: RwLock::new(HashMap::new()),
        };
        registry.register_builtins();
        registry
    }

    /// Register a worker factory by name.
    pub async fn register(
        &self,
        name: &str,
        factory: impl Fn() -> Box<dyn WorkerAgent> + Send + Sync + 'static,
    ) {
        self.workers
            .write()
            .await
            .insert(name.to_string(), Arc::new(factory));
        tracing::info!("Registered worker: {}", name);
    }

    /// Spawn a worker by name.
    pub async fn spawn(&self, name: &str) -> Option<Box<dyn WorkerAgent>> {
        self.workers
            .read()
            .await
            .get(name)
            .map(|f| f())
    }

    /// Check if a worker is registered.
    pub async fn has(&self, name: &str) -> bool {
        self.workers.read().await.contains_key(name)
    }

    /// Get all registered worker names.
    pub async fn names(&self) -> Vec<String> {
        self.workers.read().await.keys().cloned().collect()
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

            self.register(&name, move || {
                Box::new(GenericWorker::new(&name_closure, &desc_closure, tools_closure.clone(), &system_prompt_closure))
            })
            .await;
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
        let builtin_names = vec!["default".to_string(), "executing".to_string()];
        let all_names = self.names().await;
        for name in all_names {
            if !builtin_names.contains(&name) {
                self.remove(&name).await;
            }
        }

        // Reload from disk
        let configs = WorkerConfig::load_all_from_dir(workers_dir)?;
        let count = self.load_from_configs(configs).await?;
        tracing::info!("Reloaded {} worker(s) from {:?}", count, workers_dir);
        Ok(count)
    }

    /// Remove a worker by name.
    pub async fn remove(&self, name: &str) -> bool {
        self.workers.write().await.remove(name).is_some()
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
        let worker_names = self.names().await;
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
                };
                return Some((first_name.clone(), fallback_config));
            }
            return None;
        }

        // Sort by score descending
        candidates.sort_by(|a, b| b.2.cmp(&a.2));

        // Return the best candidate
        let (best_name, best_config, _) = &candidates[0];
        Some((best_name.clone(), best_config.clone()))
    }

    fn register_builtins(&mut self) {
        // Register a default fallback worker
        self.register("default", || {
            Box::new(GenericWorker::new(
                "default",
                "Default fallback worker",
                vec!["file_io".to_string()],
                "You are a default worker.",
            ))
        });

        // Register the executing worker (has tool_manager access)
        self.register("executing", || {
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
                // Note: this factory is called without a ToolManager;
                // the supervisor wires it up separately.
                Arc::new(tokio::sync::Mutex::new(
                    crate::tools::ToolManager::new(
                        std::sync::Arc::new(crate::tools::registry::ToolRegistry::new(
                            vec![],
                            std::sync::Arc::new(crate::tools::lib::TracingToolLogger),
                        )),
                    )
                )),
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

    #[tokio::test]
    async fn test_register_and_spawn() {
        let registry = WorkerRegistry::new();
        registry
            .register("test", || Box::new(GenericWorker::new("test", "Test", vec![], "You are a test worker.")))
            .await;

        assert!(registry.has("test").await);
        assert!(!registry.has("nonexistent").await);

        let worker = registry.spawn("test").await;
        assert!(worker.is_some());
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
            },
            WorkerConfig {
                name: "coding".to_string(),
                description: "Coding worker".to_string(),
                system_prompt: "You are a coder.".to_string(),
                allowed_tools: vec!["file_io".to_string()],
                priority: 1,
                max_concurrent: 2,
                enabled: true,
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
        };

        let result = registry.find_best_worker(&task, &Vec::new()).await;
        assert!(result.is_some());
    }
}
