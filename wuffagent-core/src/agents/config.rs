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
                            task_timeout_ms: if legacy.task_timeout_ms > 0 { legacy.task_timeout_ms } else { 60_000 },
                            shell_config: legacy.shell_config,
                            agents_dir: self.agents_dir.clone(),
                            agents_search_dirs: Vec::new(),
                            custom_prompts: HashMap::new(),
                            reasoning_effort: legacy.reasoning_effort,
                            trim_config: crate::trimming::config::TrimConfig::default(),
                            handoff_enabled: legacy.handoff_enabled,
                            handoff_targets: legacy.can_invoke,
                            restart_enabled: true,
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

        agents.sort_by(|a, b| a.name.cmp(&b.name));
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
mod tests;

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
    /// Timeout for each task execution (milliseconds).
    #[serde(default = "default_task_timeout_ms")]
    pub task_timeout_ms: u64,
    /// Shell configuration for this agent.
    #[serde(default)]
    pub shell_config: ShellConfig,
    /// Directory containing per-agent JSON config files.
    #[serde(default = "default_agents_dir")]
    pub agents_dir: PathBuf,
    /// Additional directories to scan for agent profiles, checked AFTER
    /// `agents_dir` (first-seen name wins). Carried by the chat path so the
    /// `handoff` tool sees the same profiles the UI agent selector does.
    #[serde(default)]
    pub agents_search_dirs: Vec<PathBuf>,
    /// Custom system prompts per agent type.
    #[serde(default)]
    pub custom_prompts: HashMap<String, String>,
    /// Reasoning effort for this agent (Off = inherit the global toggle).
    /// Also accepts legacy string values "off", "low", "medium", "high" for backward compat.
    #[serde(default)]
    pub reasoning_effort: crate::types::ReasoningEffort,
    /// Configuration for intelligent context trimming.
    #[serde(default)]
    pub trim_config: crate::trimming::config::TrimConfig,
    /// Whether this agent may hand off the session to another agent via the
    /// `handoff` tool (gated by flag, like `shell` — not via `allowed_tools`).
    /// Calling the tool ends this agent's turn; the target agent continues the
    /// same conversation with its own prompt/tools/shell/reasoning.
    #[serde(default)]
    pub handoff_enabled: bool,
    /// Agent names this agent may hand off to (empty = any enabled agent).
    /// Also accepts the legacy `can_invoke` key.
    #[serde(default, alias = "can_invoke")]
    pub handoff_targets: Vec<String>,
    /// Whether this agent may restart WuffAgent via the `restart` tool (most
    /// useful for dogfooding: edit WuffAgent's own source, rebuild, restart to
    /// load it, then resume). Defaults to true so it can be turned off per
    /// profile.
    #[serde(default = "default_true")]
    pub restart_enabled: bool,
}

fn default_enabled_agent() -> bool { true }
fn default_true() -> bool { true }
fn default_task_timeout_ms() -> u64 { 60_000 }
pub fn default_agents_dir() -> PathBuf {
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
            task_timeout_ms: 60_000,
            shell_config: ShellConfig::default(),
            agents_dir: default_agents_dir(),
            agents_search_dirs: Vec::new(),
            custom_prompts: HashMap::new(),
            reasoning_effort: crate::types::ReasoningEffort::default(),
            trim_config: crate::trimming::config::TrimConfig::default(),
            handoff_enabled: false,
            handoff_targets: Vec::new(),
            restart_enabled: true,
        }
    }
}

/// Load an enabled agent profile by name from a directory of agent JSON files.
///
/// Tries the current `AgentConfig` format first, then the legacy
/// `WorkerConfig` format (migrated, including `can_invoke` → `handoff_targets`
/// and `handoff_enabled`). Returns `None` when no file matches `name` or the
/// matched profile is not enabled.
pub fn load_agent_from_dir(dir: &Path, name: &str) -> Option<AgentConfig> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(mut cfg) = serde_json::from_str::<AgentConfig>(&content) {
            if cfg.name == name {
                if !cfg.enabled {
                    return None;
                }
                // Anchor the agents dir to the directory we actually scanned so
                // further (chained) handoffs resolve targets from the same place.
                cfg.agents_dir = dir.to_path_buf();
                return Some(cfg);
            }
            continue;
        }
        if let Ok(legacy) = serde_json::from_str::<WorkerConfig>(&content) {
            if legacy.name == name {
                if !legacy.enabled {
                    return None;
                }
                return Some(AgentConfig {
                    name: legacy.name,
                    description: legacy.description,
                    system_prompt: legacy.system_prompt,
                    allowed_tools: legacy.allowed_tools,
                    enabled: legacy.enabled,
                    task_timeout_ms: if legacy.task_timeout_ms > 0 {
                        legacy.task_timeout_ms
                    } else {
                        60_000
                    },
                    shell_config: legacy.shell_config,
                    agents_dir: dir.to_path_buf(),
                    agents_search_dirs: Vec::new(),
                    custom_prompts: HashMap::new(),
                    reasoning_effort: legacy.reasoning_effort,
                    trim_config: crate::trimming::config::TrimConfig::default(),
                    handoff_enabled: legacy.handoff_enabled,
                    handoff_targets: legacy.can_invoke,
                    restart_enabled: true,
                });
            }
        }
    }
    None
}

/// Load an enabled agent profile by name from an ORDERED list of directories
/// (primary first, then search dirs); the first directory containing the
/// profile wins.
///
/// On a hit, the returned config is anchored for CHAINED handoffs: its
/// `agents_dir` points at the directory we actually found the profile in and
/// `agents_search_dirs` holds the remaining directories (original order), so
/// the target agent's own `handoff` tool resolves targets the same way the
/// caller's did.
pub fn load_agent_from_dirs(dirs: &[PathBuf], name: &str) -> Option<AgentConfig> {
    let mut cfg = None;
    for (i, dir) in dirs.iter().enumerate() {
        if let Some(found) = load_agent_from_dir(dir, name) {
            cfg = Some((i, found));
            break;
        }
    }
    cfg.map(|(i, mut c)| {
        c.agents_dir = dirs[i].clone();
        c.agents_search_dirs = dirs[..i].iter().cloned().chain(dirs[i + 1..].iter().cloned()).collect();
        c
    })
}

impl AgentConfig {
    /// Get the shell configuration, returning a default if not explicitly set.
    pub fn get_shell_config(&self) -> ShellConfig {
        if self.shell_config.shell_enabled || !self.shell_config.allowed_commands.is_empty() {
            self.shell_config.clone()
        } else {
            ShellConfig::default()
        }
    }

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
                            task_timeout_ms: if legacy.task_timeout_ms > 0 { legacy.task_timeout_ms } else { 60_000 },
                            shell_config: legacy.shell_config,
                            agents_dir: self.agents_dir.clone(),
                            agents_search_dirs: Vec::new(),
                            custom_prompts: HashMap::new(),
                            reasoning_effort: legacy.reasoning_effort,
                            trim_config: crate::trimming::config::TrimConfig::default(),
                            handoff_enabled: legacy.handoff_enabled,
                            handoff_targets: legacy.can_invoke,
                            restart_enabled: true,
                        };
                        agents.push(config);
                    } else {
                        tracing::warn!("Failed to load agent config from {:?}", path);
                    }
                }
            }
        }

        agents.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(agents)
    }
}
