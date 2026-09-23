//! Agent profile discovery and resolution for the chat input: which
//! `agents/` dirs to scan, loading `AgentConfig`/legacy `WorkerConfig`
//! profiles, and resolving a profile's system prompt + tool policy by name.

use std::path::PathBuf;

use super::super::state::ChatApp;

impl ChatApp {
    /// The known agents directories in priority order: the config-dir
    /// `agents/` first, then the project-level `agents/` dirs (cwd, exe dir) —
    /// the same discovery set the UI agent dialog, the improvements panel (F3/F4), and the bootstrap engine use.
    pub(in crate::ui) fn agents_dirs(&self) -> Vec<PathBuf> {
        let agents_dir = self.config.file_path
            .parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| self.config.file_path.clone());
        let mut dirs = vec![agents_dir];
        if let Ok(cwd) = std::env::current_dir() {
            let d = cwd.join("agents");
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let d = exe_dir.join("agents");
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
        }
        dirs
    }

    /// Load the first matching agent profile (by `name`) from the known agents
    /// directories, handling both current `AgentConfig` and legacy `WorkerConfig`
    /// (via the core loader, which preserves every field incl. handoff settings).
    fn load_agent_config(&self, names: &[&str]) -> Option<wuffagent_core::agents::config::AgentConfig> {
        let dirs = self.agents_dirs();
        for name in names {
            if let Some(cfg) = wuffagent_core::agents::config::load_agent_from_dirs(&dirs, name) {
                return Some(cfg);
            }
        }
        None
    }

    /// Resolve the tool policy for an agent profile by name. An empty name
    /// ("Auto") or a profile not found yields an unrestricted policy (all tools
    /// + allow-all shell, no handoff).
    pub(super) fn resolve_tool_policy(&self, agent_name: &str) -> wuffagent_core::types::ChatToolPolicy {
        let names: Vec<&str> = if agent_name.is_empty() {
            vec!["general", "generalist"]
        } else {
            vec![agent_name]
        };
        let policy = self.load_agent_config(&names);
        match policy {
            Some(cfg) => wuffagent_core::types::ChatToolPolicy {
                allowed_tools: cfg.allowed_tools,
                shell_config: cfg.shell_config,
                agent_name: cfg.name,
                handoff_enabled: cfg.handoff_enabled,
                handoff_targets: cfg.handoff_targets,
                restart_enabled: cfg.restart_enabled,
                reasoning_effort: cfg.reasoning_effort,
                trim_config: cfg.trim_config,
            },
            None => wuffagent_core::types::ChatToolPolicy::unrestricted(),
        }
    }

    /// Resolve the system prompt for an agent profile by name (empty = "Auto"
    /// -> the general profile).
    pub(super) fn resolve_agent_prompt(&self, agent_name: &str) -> String {
        let names: Vec<&str> = if agent_name.is_empty() {
            vec!["general", "generalist"]
        } else {
            vec![agent_name]
        };
        let prompt = self.load_agent_system_prompt(&names);
        if prompt.is_empty() {
            tracing::warn!("No agent system prompt found - chat will run without one");
        }
        prompt
    }

    /// Load the system prompt of the first matching agent profile.
    /// Searches the same agents directories as `get_agent_names` and
    /// matches the profile's `name` field (not the file name).
    /// Returns an empty string when no candidate profile exists.
    fn load_agent_system_prompt(&self, names: &[&str]) -> String {
        let dirs = self.agents_dirs();

        let mut prompt = String::new();
        for dir in &dirs {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(cfg) = serde_json::from_str::<wuffagent_core::agents::config::AgentConfig>(&content) {
                            if names.iter().any(|n| cfg.name == *n) {
                                prompt = cfg.system_prompt;
                                break;
                            }
                        } else if let Ok(cfg) = serde_json::from_str::<wuffagent_core::agents::config::WorkerConfig>(&content) {
                            if names.iter().any(|n| cfg.name == *n) {
                                prompt = cfg.system_prompt;
                                break;
                            }
                        }
                    }
                }
            }
            if !prompt.is_empty() {
                break;
            }
        }

        // The handoff hint is appended by `Agent::build_system_prompt`
        // when the profile has `handoff_enabled` — mirroring it here would
        // advertise handoffs the chat agent is not allowed to make.

        prompt
    }

    /// Return the list of agent names from all known agents directories.
    pub(super) fn get_agent_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Helper: scan a directory for agent names
        let mut scan_dir = |dir: PathBuf| {
            if dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(cfg) = serde_json::from_str::<wuffagent_core::agents::config::AgentConfig>(&content) {
                                if cfg.enabled && seen.insert(cfg.name.clone()) {
                                    names.push(cfg.name);
                                }
                            } else if let Ok(cfg) = serde_json::from_str::<wuffagent_core::agents::config::WorkerConfig>(&content) {
                                if cfg.enabled && seen.insert(cfg.name.clone()) {
                                    names.push(cfg.name);
                                }
                            }
                        }
                    }
                }
            }
        };

        // Scan all known agents directories (config dir first, then the
        // project-level `agents/` dirs) — same discovery set as the UI agent
        // dialog, so the selector lists the same profiles it can edit.
        for dir in self.agents_dirs() {
            scan_dir(dir);
        }

        names
    }
}
