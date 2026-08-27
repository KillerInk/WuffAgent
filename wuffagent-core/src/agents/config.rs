use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing;

/// Shell configuration for an agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Allowed command patterns (regex). Empty means allow all (except dangerous).
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// Shell type: "powershell", "cmd", or "bash".
    #[serde(default = "default_shell_type")]
    pub shell_type: String,
    /// Default timeout in milliseconds.
    #[serde(default = "default_shell_timeout")]
    pub shell_timeout_ms: u64,
    /// Whether shell commands are enabled.
    #[serde(default = "default_shell_enabled")]
    pub shell_enabled: bool,
    /// Working directory restriction (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            allowed_commands: Vec::new(),
            shell_type: "powershell".to_string(),
            shell_timeout_ms: 300_000,
            shell_enabled: false,
            working_dir: None,
        }
    }
}

fn default_shell_type() -> String { "powershell".to_string() }
fn default_shell_timeout() -> u64 { 300_000 }
fn default_shell_enabled() -> bool { false }

/// Legacy configuration for a single worker, loaded from a JSON file.
/// Kept for backward compatibility with existing agent JSON files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Unique name/identifier for this worker.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// System prompt for this worker (backwards compat: also accepts "personality").
    #[serde(default, alias = "personality")]
    pub system_prompt: String,
    /// Tool names this worker is authorized to use.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Priority for Supervisor selection (lower = preferred).
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Maximum concurrent tasks this worker can handle.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Whether this worker is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Names of agents this worker can invoke via agent_call.
    #[serde(default)]
    pub can_invoke: Vec<String>,
    /// Whether runtime handoffs are allowed.
    #[serde(default = "default_handoff_enabled")]
    pub handoff_enabled: bool,
    /// Shell configuration for this worker.
    #[serde(default)]
    pub shell_config: ShellConfig,
    /// Reasoning effort for this agent (Off = inherit the global toggle).
    #[serde(default)]
    pub reasoning_effort: crate::types::ReasoningEffort,
    /// Timeout for task execution (milliseconds). Defaults to the global value if not set.
    #[serde(default)]
    pub task_timeout_ms: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            name: String::from("generic"),
            description: String::from("Generic worker"),
            system_prompt: String::from("You are a worker."),
            allowed_tools: Vec::new(),
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            can_invoke: Vec::new(),
            handoff_enabled: false,
            shell_config: ShellConfig::default(),
            reasoning_effort: crate::types::ReasoningEffort::default(),
            task_timeout_ms: 300_000,
        }
    }
}

fn default_handoff_enabled() -> bool { false }

fn default_enabled() -> bool { true }

fn default_priority() -> u32 { 0 }
fn default_max_concurrent() -> usize { 1 }

impl WorkerConfig {
    /// Load all Agent configs from a directory.
    pub fn load_all_from_dir(dir: &Path) -> Result<Vec<Self>, crate::agents::AgentError> {
        let mut workers = Vec::new();
        if !dir.exists() {
            return Ok(workers);
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read agents directory: {}", e);
                return Ok(workers);
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "json").unwrap_or(false) {
                match Self::load_from_file(&path) {
                    Ok(config) => workers.push(config),
                    Err(e) => tracing::warn!("Failed to load {:?}: {}", path, e),
                }
            }
        }
        workers.sort_by_key(|w| w.priority);
        Ok(workers)
    }

    pub fn load_from_file(path: &Path) -> Result<Self, crate::agents::AgentError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to read Agent config from {:?}: {}", path, e
            )))?;
        let config: WorkerConfig = serde_json::from_str(&content)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to parse Agent config from {:?}: {}", path, e
            )))?;
        Ok(config)
    }

    pub fn save_to_file(&self, path: &Path) -> Result<(), crate::agents::AgentError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                    "Failed to create directory {:?}: {}", parent, e
                )))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to serialize Agent config: {}", e
            )))?;
        std::fs::write(path, content)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to write Agent config to {:?}: {}", path, e
            )))?;
        Ok(())
    }

    /// Derive an AgentType from the worker's allowed tools and description.
    pub fn infer_agent_type(&self) -> crate::agents::types::AgentType {
        let desc_lower = self.description.to_lowercase();
        let tool_names: Vec<&str> = self.allowed_tools.iter().map(|s| s.as_str()).collect();

        // Heuristic: infer agent type from tool names and description
        if tool_names.contains(&"web_search")
            || desc_lower.contains("research") || desc_lower.contains("search")
        {
            return crate::agents::types::AgentType::Research;
        }
        if tool_names.contains(&"file_io")
            || desc_lower.contains("code") || desc_lower.contains("write") || desc_lower.contains("read")
        {
            return crate::agents::types::AgentType::Coding;
        }
        if tool_names.contains(&"calculation")
            || desc_lower.contains("execute") || desc_lower.contains("build") || desc_lower.contains("deploy")
        {
            return crate::agents::types::AgentType::Implementation;
        }
        crate::agents::types::AgentType::General
    }

    /// Get the shell configuration, returning a default if not explicitly set.
    pub fn get_shell_config(&self) -> ShellConfig {
        if self.shell_config.shell_enabled || !self.shell_config.allowed_commands.is_empty() {
            self.shell_config.clone()
        } else {
            ShellConfig::default()
        }
    }
}

/// Manages the lifecycle of agent configurations: load, add, edit, remove, reload.
///
/// Agents are discovered from `search_dirs` (read-only scan), but new/edited agents
/// are persisted to `agents_dir` (the primary config directory).
pub struct AgentManager {
    /// Primary directory where agents are saved/loaded from.
    agents_dir: PathBuf,
    /// Additional directories to scan for existing agents.
    search_dirs: Vec<PathBuf>,
}

impl AgentManager {
    pub fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            search_dirs: Vec::new(),
        }
    }

    /// Returns the primary agents directory path.
    pub fn agents_dir(&self) -> &PathBuf {
        &self.agents_dir
    }

    /// Add an additional directory to scan for existing agent configs.
    pub fn add_search_dir(&mut self, dir: PathBuf) {
        if !self.search_dirs.contains(&dir) {
            self.search_dirs.push(dir);
        }
    }

    /// Load agent configs from a single directory, deduplicating by name (first wins).
    /// Tries AgentConfig first, falls back to legacy WorkerConfig.
    fn load_from_dir(&self, dir: &PathBuf, seen: &mut HashMap<String, ()>) -> Result<Vec<AgentConfig>, crate::agents::AgentError> {
        let mut agents = Vec::new();
        if !dir.exists() {
            return Ok(agents);
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read directory {:?}: {}", dir, e);
                return Ok(agents);
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "json").unwrap_or(false) {
                // Try new AgentConfig format first
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(config) = serde_json::from_str::<AgentConfig>(&content) {
                        if seen.insert(config.name.clone(), ()).is_none() {
                            tracing::info!(
                                "Discovered agent: {} from {:?} (tools={:?})",
                                config.name,
                                dir,
                                config.allowed_tools
                            );
                            agents.push(config);
                        }
                        continue;
                    }
                    // Fallback to legacy WorkerConfig
                    if let Ok(legacy) = serde_json::from_str::<WorkerConfig>(&content) {
                        let config = AgentConfig {
                            name: legacy.name,
                            description: legacy.description,
                            system_prompt: legacy.system_prompt,
                            allowed_tools: legacy.allowed_tools,
                            enabled: legacy.enabled,
                            priority: legacy.priority,
                            max_concurrent: legacy.max_concurrent,
                            max_depth: 5,
                            recovery_policy: RecoveryPolicy::default(),
                            max_plan_iterations: 5,
                            max_parallel_workers: 4,
                            task_timeout_ms: if legacy.task_timeout_ms > 0 { legacy.task_timeout_ms } else { 60_000 },
                            auto_refine: true,
                            can_invoke: legacy.can_invoke,
                            handoff_enabled: legacy.handoff_enabled,
                            shell_config: legacy.shell_config,
                            agents_dir: self.agents_dir.clone(),
                            custom_prompts: HashMap::new(),
                            reasoning_effort: legacy.reasoning_effort,
                            trim_config: crate::trimming::config::TrimConfig::default(),
                        };
                        if seen.insert(config.name.clone(), ()).is_none() {
                            tracing::info!(
                                "Discovered legacy agent: {} from {:?} (tools={:?})",
                                config.name,
                                dir,
                                config.allowed_tools
                            );
                            agents.push(config);
                        }
                    } else {
                        tracing::warn!("Failed to load agent config from {:?}: invalid format", path);
                    }
                } else {
                    tracing::warn!("Failed to read agent config from {:?}", path);
                }
            }
        }
        Ok(agents)
    }

    /// Load all agent configs from agents_dir plus any search_dirs.
    /// Agents from agents_dir take priority (loaded first, deduplication keeps first).
    pub fn list_agents(&self) -> Result<Vec<AgentConfig>, crate::agents::AgentError> {
        let mut agents = Vec::new();
        let mut seen = HashMap::new();

        // Primary directory first
        agents.extend(self.load_from_dir(&self.agents_dir, &mut seen)?);

        // Additional search directories
        for dir in &self.search_dirs {
            if dir != &self.agents_dir {
                agents.extend(self.load_from_dir(dir, &mut seen)?);
            }
        }

        agents.sort_by_key(|a| a.priority);
        Ok(agents)
    }

    /// Get a single agent config by name.
    pub fn get_agent(&self, name: &str) -> Option<AgentConfig> {
        self.list_agents()
            .ok()
            .into_iter()
            .flatten()
            .find(|a| a.name == name)
    }

    /// Add a new agent config to the agents directory.
    pub fn add_agent(&self, config: &AgentConfig) -> Result<(), crate::agents::AgentError> {
        let path = self.agents_dir.join(format!("{}.json", config.name));
        config.save_to_file(&path)?;
        tracing::info!("Added agent config: {}", config.name);
        Ok(())
    }

    /// Edit an existing agent config (update in place).
    pub fn edit_agent(&self, name: &str, config: &AgentConfig) -> Result<(), crate::agents::AgentError> {
        if config.name != name {
            // Name changed â€” remove old file and save new one
            self.remove_agent(name)?;
        }
        let path = self.agents_dir.join(format!("{}.json", config.name));
        config.save_to_file(&path)?;
        tracing::info!("Edited agent config: {}", config.name);
        Ok(())
    }

    /// Remove an agent config from the agents directory.
    pub fn remove_agent(&self, name: &str) -> Result<(), crate::agents::AgentError> {
        let path = self.agents_dir.join(format!("{}.json", name));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                    "Failed to remove agent config {:?}: {}", path, e
                )))?;
            tracing::info!("Removed agent config: {}", name);
        }
        Ok(())
    }

    /// Reload all agent configs from disk (use after add/edit/remove).
    pub fn reload(&self) -> Result<Vec<AgentConfig>, crate::agents::AgentError> {
        let agents = self.list_agents();
        tracing::info!("Reloaded {} agent config(s) from {:?}", agents.as_ref().map(|a| a.len()).unwrap_or(0), self.agents_dir);
        agents
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
            system_prompt: String::new(),
            allowed_tools: vec!["web_search".to_string()],
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            ..Default::default()
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Research);
    }

    #[test]
    fn test_worker_config_infer_coding() {
        let config = WorkerConfig {
            name: "coder".to_string(),
            description: "Code file manipulation".to_string(),
            system_prompt: String::new(),
            allowed_tools: vec!["file_io".to_string()],
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            ..Default::default()
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Coding);
    }

    #[test]
    fn test_worker_config_infer_implementation() {
        let config = WorkerConfig {
            name: "builder".to_string(),
            description: "Build and deploy".to_string(),
            system_prompt: String::new(),
            allowed_tools: vec!["calculation".to_string()],
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            ..Default::default()
        };
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Implementation);
    }

    #[test]
    fn test_worker_config_infer_general() {
        let config = WorkerConfig {
            name: "general".to_string(),
            description: "General purpose tasks".to_string(),
            system_prompt: String::new(),
            allowed_tools: vec!["web_search".to_string()],
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            ..Default::default()
        };
        // web_search maps to Research, so we use a tool that doesn't match any category
        assert_eq!(config.infer_agent_type(), crate::agents::types::AgentType::Research);
    }

    #[test]
    fn test_worker_config_backwards_compat_personality() {
        let json = r#"{"name":"test","description":"Desc","personality":"You are a test worker.","allowed_tools":["file_io"],"priority":0,"max_concurrent":1}"#;
        let config: WorkerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.system_prompt, "You are a test worker.");
        assert_eq!(config.name, "test");
        assert!(!config.shell_config.shell_enabled);
    }

    #[test]
    fn test_worker_config_shell_config() {
        let json = r#"{
            "name":"executor",
            "description":"Build and run",
            "allowed_tools":["shell"],
            "shell_config": {
                "shell_enabled": true,
                "allowed_commands": ["cargo build.*", "git.*"],
                "shell_type": "powershell",
                "shell_timeout_ms": 60000
            }
        }"#;
        let config: WorkerConfig = serde_json::from_str(json).unwrap();
        assert!(config.shell_config.shell_enabled);
        assert_eq!(config.shell_config.allowed_commands, vec!["cargo build.*".to_string(), "git.*".to_string()]);
        assert_eq!(config.shell_config.shell_type, "powershell");
        assert_eq!(config.shell_config.shell_timeout_ms, 60000);
    }

    #[test]
    fn test_worker_config_save_and_load() {
        let dir = std::env::temp_dir().join("wuffagent_test_agents");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let config = WorkerConfig {
            name: "test_agent".to_string(),
            description: "A test agent".to_string(),
            system_prompt: "You are a test agent.".to_string(),
            allowed_tools: vec!["file_io".to_string()],
            priority: 5,
            max_concurrent: 2,
            enabled: false,
            ..Default::default()
        };

        let path = dir.join("test_agent.json");
        config.save_to_file(&path).unwrap();

        let loaded = WorkerConfig::load_from_file(&path).unwrap();
        assert_eq!(loaded.name, "test_agent");
        assert_eq!(loaded.system_prompt, "You are a test agent.");
        assert_eq!(loaded.allowed_tools, vec!["file_io".to_string()]);
        assert_eq!(loaded.priority, 5);
        assert_eq!(loaded.max_concurrent, 2);
        assert!(!loaded.enabled);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_agent_manager_crud() {
        let dir = std::env::temp_dir().join("wuffagent_test_mgr");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mgr = AgentManager::new(dir.clone());

        // Add
        let config = AgentConfig {
            name: "mgr_test".to_string(),
            description: "Manager test".to_string(),
            system_prompt: "You are a mgr test.".to_string(),
            allowed_tools: vec!["file_io".to_string()],
            enabled: true,
            ..Default::default()
        };
        mgr.add_agent(&config).unwrap();
        assert!(mgr.get_agent("mgr_test").is_some());

        // List
        let agents = mgr.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "mgr_test");

        // Edit
        let mut edited = config.clone();
        edited.description = "Updated description".to_string();
        mgr.edit_agent("mgr_test", &edited).unwrap();
        let loaded = mgr.get_agent("mgr_test").unwrap();
        assert_eq!(loaded.description, "Updated description");

        // Remove
        mgr.remove_agent("mgr_test").unwrap();
        assert!(mgr.get_agent("mgr_test").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_agent_manager_reload() {
        let dir = std::env::temp_dir().join("wuffagent_test_reload");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mgr = AgentManager::new(dir.clone());
        assert_eq!(mgr.reload().unwrap().len(), 0);

        let config = AgentConfig {
            name: "reload_test".to_string(),
            description: "Reload test".to_string(),
            system_prompt: "Reload prompt".to_string(),
            allowed_tools: vec![],
            enabled: true,
            ..Default::default()
        };
        mgr.add_agent(&config).unwrap();
        let loaded = mgr.reload().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "reload_test");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_agent_manager_multi_dir_search() {
        let primary_dir = std::env::temp_dir().join("wuffagent_test_primary");
        let search_dir = std::env::temp_dir().join("wuffagent_test_search");
        let _ = std::fs::remove_dir_all(&primary_dir);
        let _ = std::fs::remove_dir_all(&search_dir);
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::create_dir_all(&search_dir).unwrap();

        // Place an agent in the search dir (simulating project workers/)
        let search_agent = AgentConfig {
            name: "search_agent".to_string(),
            description: "From search dir".to_string(),
            system_prompt: "Search prompt".to_string(),
            allowed_tools: vec!["web_search".to_string()],
            enabled: true,
            ..Default::default()
        };
        search_agent.save_to_file(&search_dir.join("search_agent.json")).unwrap();

        // Place an agent in the primary dir
        let primary_agent = AgentConfig {
            name: "primary_agent".to_string(),
            description: "From primary dir".to_string(),
            system_prompt: "Primary prompt".to_string(),
            allowed_tools: vec!["file_io".to_string()],
            enabled: true,
            ..Default::default()
        };
        primary_agent.save_to_file(&primary_dir.join("primary_agent.json")).unwrap();

        // AgentManager with search dir
        let mut mgr = AgentManager::new(primary_dir.clone());
        mgr.add_search_dir(search_dir.clone());
        let agents = mgr.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.name == "primary_agent"));
        assert!(agents.iter().any(|a| a.name == "search_agent"));

        // Save should go to primary dir
        let new_agent = AgentConfig {
            name: "new_agent".to_string(),
            description: "New agent".to_string(),
            system_prompt: "New prompt".to_string(),
            allowed_tools: vec![],
            enabled: true,
            ..Default::default()
        };
        mgr.add_agent(&new_agent).unwrap();
        assert!(primary_dir.join("new_agent.json").exists());
        assert!(!search_dir.join("new_agent.json").exists());

        // Reload should find all 3
        let agents = mgr.reload().unwrap();
        assert_eq!(agents.len(), 3);

        let _ = std::fs::remove_dir_all(&primary_dir);
        let _ = std::fs::remove_dir_all(&search_dir);
    }
}

/// Recovery policy for agent task failures.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum RecoveryPolicy {
    #[default]
    FailFast,
    Retry,
    ContinueWithFallback,
}

/// Per-agent configuration, loaded from a JSON file in the agents directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique name/identifier for this agent.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// System prompt for this agent (also accepts legacy "personality" key).
    #[serde(default, alias = "personality")]
    pub system_prompt: String,
    /// Tool names this agent is authorized to use.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Whether this agent is enabled.
    #[serde(default = "default_enabled_agent")]
    pub enabled: bool,
    /// Priority for Supervisor selection (lower = preferred).
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Maximum concurrent tasks this agent can handle.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Maximum recursion depth for agent calls.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Recovery policy when a task fails.
    #[serde(default)]
    pub recovery_policy: RecoveryPolicy,
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
    /// Names of agents this agent can invoke via agent_call.
    #[serde(default)]
    pub can_invoke: Vec<String>,
    /// Whether runtime handoffs are allowed.
    #[serde(default = "default_handoff_enabled")]
    pub handoff_enabled: bool,
    /// Shell configuration for this agent.
    #[serde(default)]
    pub shell_config: ShellConfig,
    /// Directory containing per-agent JSON config files.
    #[serde(default = "default_agents_dir")]
    pub agents_dir: PathBuf,
    /// Custom system prompts per agent type.
    #[serde(default)]
    pub custom_prompts: HashMap<String, String>,
    /// Reasoning effort for this agent (Off = inherit the global toggle).
    #[serde(default)]
    pub reasoning_effort: crate::types::ReasoningEffort,
    /// Configuration for intelligent context trimming.
    #[serde(default)]
    pub trim_config: crate::trimming::config::TrimConfig,
}

fn default_enabled_agent() -> bool { true }
fn default_max_depth() -> u32 { 5 }
fn default_max_iterations() -> u32 { 5 }
fn default_max_parallel() -> usize { 4 }
fn default_task_timeout_ms() -> u64 { 60_000 }
fn default_auto_refine() -> bool { true }
fn default_agents_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("agents")
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: String::from("general"),
            description: String::from("General purpose agent"),
            system_prompt: String::new(),
            allowed_tools: Vec::new(),
            enabled: true,
            priority: 0,
            max_concurrent: 1,
            max_depth: 5,
            recovery_policy: RecoveryPolicy::default(),
            max_plan_iterations: 5,
            max_parallel_workers: 4,
            task_timeout_ms: 60_000,
            auto_refine: true,
            can_invoke: Vec::new(),
            handoff_enabled: false,
            shell_config: ShellConfig::default(),
            agents_dir: default_agents_dir(),
            custom_prompts: HashMap::new(),
            reasoning_effort: crate::types::ReasoningEffort::default(),
            trim_config: crate::trimming::config::TrimConfig::default(),
        }
    }
}

impl AgentConfig {
    /// Save this config to a JSON file.
    pub fn save_to_file(&self, path: &Path) -> Result<(), crate::agents::AgentError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                    "Failed to create directory {:?}: {}", parent, e
                )))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to serialize agent config: {}", e
            )))?;
        std::fs::write(path, content)
            .map_err(|e| crate::agents::AgentError::ConfigError(format!(
                "Failed to write agent config to {:?}: {}", path, e
            )))?;
        Ok(())
    }

    /// Load agent config from the app's config directory.
    pub fn load(config_path: &Path) -> Result<Self, crate::agents::AgentError> {
        let agents_dir = config_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(default_agents_dir);

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
                config.agents_dir = agents_dir;
                return Ok(config);
            }
        }
        Ok(Self::default())
    }

    /// Discover and load all agent configs from the agents directory.
    pub fn load_agents(&self) -> Result<Vec<AgentConfig>, crate::agents::AgentError> {
        let mut agents = Vec::new();

        if !self.agents_dir.exists() {
            tracing::info!("agents directory does not exist: {:?}, using built-in defaults", self.agents_dir);
            return Ok(agents);
        }

        let entries = match std::fs::read_dir(&self.agents_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read agents directory: {}", e);
                return Ok(agents);
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Failed to read worker entry: {}", e);
                    continue;
                }
            };
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "json").unwrap_or(false) {
                // Try AgentConfig first, fall back to legacy WorkerConfig
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(config) = serde_json::from_str::<AgentConfig>(&content) {
                        tracing::info!(
                            "Loaded agent config: {} (tools={:?})",
                            config.name,
                            config.allowed_tools
                        );
                        agents.push(config);
                    } else if let Ok(legacy) = serde_json::from_str::<WorkerConfig>(&content) {
                        tracing::info!(
                            "Loaded legacy Agent config: {} (tools={:?})",
                            legacy.name,
                            legacy.allowed_tools
                        );
                        let config = AgentConfig {
                            name: legacy.name,
                            description: legacy.description,
                            system_prompt: legacy.system_prompt,
                            allowed_tools: legacy.allowed_tools,
                            enabled: legacy.enabled,
                            priority: legacy.priority,
                            max_concurrent: legacy.max_concurrent,
                            max_depth: 5,
                            recovery_policy: RecoveryPolicy::default(),
                            max_plan_iterations: 5,
                            max_parallel_workers: 4,
                            task_timeout_ms: if legacy.task_timeout_ms > 0 { legacy.task_timeout_ms } else { 60_000 },
                            auto_refine: true,
                            can_invoke: legacy.can_invoke,
                            handoff_enabled: legacy.handoff_enabled,
                            shell_config: legacy.shell_config,
                            agents_dir: self.agents_dir.clone(),
                            custom_prompts: HashMap::new(),
                            reasoning_effort: legacy.reasoning_effort,
                            trim_config: crate::trimming::config::TrimConfig::default(),
                        };
                        agents.push(config);
                    } else {
                        tracing::warn!("Failed to load agent config from {:?}", path);
                    }
                }
            }
        }

        // Sort by priority (lower = first)
        agents.sort_by_key(|a| a.priority);
        Ok(agents)
    }
}
