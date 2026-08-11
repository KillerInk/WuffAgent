use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing;

/// Configuration for a single worker, loaded from a JSON file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Unique name/identifier for this worker.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// System prompt / personality for this worker.
    #[serde(default)]
    pub personality: String,
    /// Tool names this worker is authorized to use.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Priority for Supervisor selection (lower = preferred).
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Maximum concurrent tasks this worker can handle.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            name: String::from("generic"),
            description: String::from("Generic worker"),
            personality: String::from("You are a worker."),
            allowed_tools: Vec::new(),
            priority: 0,
            max_concurrent: 1,
        }
    }
}

fn default_priority() -> u32 { 0 }
fn default_max_concurrent() -> usize { 1 }

impl WorkerConfig {
    pub fn load_from_file(path: &Path) -> Result<Self, crate::agents::AgentError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to read worker config from {:?}: {}", path, e
            )))?;
        let config: WorkerConfig = serde_json::from_str(&content)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to parse worker config from {:?}: {}", path, e
            )))?;
        Ok(config)
    }

    /// Derive an AgentType from the worker's allowed tools and description.
    pub fn infer_agent_type(&self) -> crate::agents::types::AgentType {
        let desc_lower = self.description.to_lowercase();
        let tool_names: Vec<&str> = self.allowed_tools.iter().map(|s| s.as_str()).collect();

        // Heuristic: infer agent type from tool names and description
        if tool_names.iter().any(|t| *t == "web_search")
            || desc_lower.contains("research") || desc_lower.contains("search")
        {
            return crate::agents::types::AgentType::Research;
        }
        if tool_names.iter().any(|t| *t == "file_io")
            || desc_lower.contains("code") || desc_lower.contains("write") || desc_lower.contains("read")
        {
            return crate::agents::types::AgentType::Coding;
        }
        if tool_names.iter().any(|t| *t == "calculation")
            || desc_lower.contains("execute") || desc_lower.contains("build") || desc_lower.contains("deploy")
        {
            return crate::agents::types::AgentType::Implementation;
        }
        crate::agents::types::AgentType::General
    }
}

/// Global agent pipeline configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum feedback loop iterations before giving up.
    #[serde(default = "default_max_iterations")]
    pub max_plan_iterations: u32,
    /// Maximum concurrent worker tasks across all workers.
    #[serde(default = "default_max_parallel")]
    pub max_parallel_workers: usize,
    /// Timeout for each task execution (milliseconds).
    #[serde(default = "default_task_timeout_ms")]
    pub task_timeout_ms: u64,
    /// Whether to enable automatic plan refinement.
    #[serde(default = "default_auto_refine")]
    pub auto_refine: bool,
    /// Directory containing per-worker JSON config files.
    #[serde(default = "default_workers_dir")]
    pub workers_dir: PathBuf,
    /// Custom system prompts per agent type.
    #[serde(default)]
    pub custom_prompts: HashMap<String, String>,
}

fn default_max_iterations() -> u32 { 5 }
fn default_max_parallel() -> usize { 4 }
fn default_task_timeout_ms() -> u64 { 60_000 }
fn default_auto_refine() -> bool { true }
fn default_workers_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("workers")
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_plan_iterations: 5,
            max_parallel_workers: 4,
            task_timeout_ms: 60_000,
            auto_refine: true,
            workers_dir: default_workers_dir(),
            custom_prompts: HashMap::new(),
        }
    }
}

impl AgentConfig {
    /// Load agent config from the app's config directory.
    pub fn load(config_path: &Path) -> Result<Self, crate::agents::AgentError> {
        let workers_dir = config_path
            .parent()
            .map(|p| p.join("workers"))
            .unwrap_or_else(|| default_workers_dir());

        // Check if there's an agent-specific config file
        let agent_config_path = config_path.parent().map(|p| p.join("agent.json"));
        if let Some(path) = agent_config_path {
            if path.exists() {
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                        "Failed to read agent config: {}", e
                    )))?;
                let mut config: AgentConfig = serde_json::from_str(&content)
                    .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                        "Failed to parse agent config: {}", e
                    )))?;
                config.workers_dir = workers_dir;
                return Ok(config);
            }
        }
        Ok(Self::default())
    }

    /// Discover and load all worker configs from the workers directory.
    pub fn load_workers(&self) -> Result<Vec<WorkerConfig>, crate::agents::AgentError> {
        let mut workers = Vec::new();

        if !self.workers_dir.exists() {
            tracing::info!("Workers directory does not exist: {:?}, using built-in defaults", self.workers_dir);
            return Ok(workers);
        }

        let mut entries = match std::fs::read_dir(&self.workers_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read workers directory: {}", e);
                return Ok(workers);
            }
        };

        while let Some(entry) = entries.next() {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Failed to read worker entry: {}", e);
                    continue;
                }
            };
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "json").unwrap_or(false) {
                match WorkerConfig::load_from_file(&path) {
                    Ok(config) => {
                        tracing::info!(
                            "Loaded worker config: {} (type={:?}, tools={:?})",
                            config.name,
                            config.infer_agent_type(),
                            config.allowed_tools
                        );
                        workers.push(config);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load worker config from {:?}: {}", path, e);
                    }
                }
            }
        }

        // Sort by priority (lower = first)
        workers.sort_by_key(|w| w.priority);
        Ok(workers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_config_infer_research() {
        let config = WorkerConfig {
            name: "researcher".to_string(),
            description: "Web research and search".to_string(),
            personality: String::new(),
            allowed_tools: vec!["web_search".to_string()],
            priority: 0,
            max_concurrent: 1,
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Research);
    }

    #[test]
    fn test_worker_config_infer_coding() {
        let config = WorkerConfig {
            name: "coder".to_string(),
            description: "Code file manipulation".to_string(),
            personality: String::new(),
            allowed_tools: vec!["file_io".to_string()],
            priority: 0,
            max_concurrent: 1,
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Coding);
    }

    #[test]
    fn test_worker_config_infer_implementation() {
        let config = WorkerConfig {
            name: "builder".to_string(),
            description: "Build and deploy".to_string(),
            personality: String::new(),
            allowed_tools: vec!["calculation".to_string(), "file_io".to_string()],
            priority: 0,
            max_concurrent: 1,
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Implementation);
    }

    #[test]
    fn test_worker_config_infer_general() {
        let config = WorkerConfig {
            name: "general".to_string(),
            description: "General purpose tasks".to_string(),
            personality: String::new(),
            allowed_tools: vec!["file_io".to_string(), "web_search".to_string()],
            priority: 0,
            max_concurrent: 1,
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::General);
    }
}
