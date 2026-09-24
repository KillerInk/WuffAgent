//! AgentManager: the agents/*.json lifecycle - discovery across
//! directories, CRUD, prompt-history snapshots, revert, rename.
//!
//! The data types (AgentConfig, WorkerConfig) live in super::config;
//! this module owns the file-system state machine around them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing;

use super::config::{AgentConfig, WorkerConfig};

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

    /// The additional (read-only) search directories, in discovery order.
    ///
    /// The UI uses this to show an agent's prompt history (F4) across every
    /// directory the selector can see: a profile living in a search dir has
    /// its snapshots next to its file, not in the primary dir.
    pub fn search_dirs(&self) -> &[PathBuf] {
        &self.search_dirs
    }

    /// Load agent configs from a single directory, deduplicating by name (first wins).
    /// Tries AgentConfig first, falls back to legacy WorkerConfig.
    fn load_from_dir(
        &self,
        dir: &PathBuf,
        seen: &mut HashMap<String, ()>,
    ) -> Result<Vec<AgentConfig>, crate::agents::AgentError> {
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
                            task_timeout_ms: if legacy.task_timeout_ms > 0 {
                                legacy.task_timeout_ms
                            } else {
                                60_000
                            },
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
                        tracing::warn!(
                            "Failed to load agent config from {:?}: invalid format",
                            path
                        );
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
    ///
    /// Snapshots the CURRENT `agents_dir/<name>.json` into
    /// [`Self::history_dir`] BEFORE overwriting it (F4), so every edit — from
    /// the improvements-panel approve path or the manual agent editor, both of
    /// which funnel through this method — is reversible via
    /// [`Self::revert_agent`]. On a rename the OLD file is snapshotted under
    /// its old name first.
    pub fn edit_agent(
        &self,
        name: &str,
        config: &AgentConfig,
    ) -> Result<(), crate::agents::AgentError> {
        if config.name != name {
            // Name changed — snapshot the old file under its old name, then
            // remove it and save the new one.
            self.snapshot_agent(name);
            self.remove_agent(name)?;
        }
        let path = self.agents_dir.join(format!("{}.json", config.name));
        // Snapshot the current file before it is overwritten.
        self.snapshot_agent(&config.name);
        config.save_to_file(&path)?;
        tracing::info!("Edited agent config: {}", config.name);
        Ok(())
    }

    /// Remove an agent config from the agents directory.
    pub fn remove_agent(&self, name: &str) -> Result<(), crate::agents::AgentError> {
        let path = self.agents_dir.join(format!("{}.json", name));
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                crate::agents::AgentError::ConfigError(format!(
                    "Failed to remove agent config {:?}: {}",
                    path, e
                ))
            })?;
            tracing::info!("Removed agent config: {}", name);
        }
        Ok(())
    }

    /// Maximum prompt-history snapshots kept per agent (oldest are pruned).
    pub const HISTORY_SNAPSHOTS_KEEP: usize = 20;

    /// Directory holding prompt-history snapshots: `agents_dir/history/`.
    fn history_dir(&self) -> PathBuf {
        self.agents_dir.join("history")
    }

    /// Parse `(unix_ts, same_second_seq)` out of the tail of a history
    /// filename after the `<name>-` prefix has been stripped, i.e.
    /// `<unixts>` or `<unixts>-<seq>`. Non-numeric tails sort as (0, 0),
    /// before all real snapshots.
    fn history_file_order(ts_seq: &str) -> (u64, u32) {
        let mut parts = ts_seq.splitn(2, '-');
        let ts = parts
            .next()
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0);
        let seq = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        (ts, seq)
    }

    /// Copy `agents_dir/<name>.json` into the history dir, keeping at most
    /// [`Self::HISTORY_SNAPSHOTS_KEEP`] snapshots per agent.
    ///
    /// No-op when the agent file does not exist (nothing to roll back to).
    /// Failures are logged but never abort the caller: history is a safety
    /// net, not a correctness dependency of the edit itself.
    fn snapshot_agent(&self, name: &str) {
        self.snapshot_file(name, &self.agents_dir.join(format!("{}.json", name)));
    }

    /// Copy `src` into the history dir under `name` (keeping at most
    /// [`Self::HISTORY_SNAPSHOTS_KEEP`] snapshots). No-op when `src` does not
    /// exist; failures are logged but never abort the caller.
    fn snapshot_file(&self, name: &str, src: &Path) {
        if !src.is_file() {
            return;
        }
        let hist = self.history_dir();
        if std::fs::create_dir_all(&hist).is_err() {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dst = self.next_snapshot_path(name, now);
        if let Err(e) = std::fs::copy(src, &dst) {
            tracing::warn!("Failed to snapshot agent '{}' history: {}", name, e);
            return;
        }
        tracing::info!("Snapshotted agent config '{}': {:?}", name, dst);
        self.prune_history(name);
    }

    /// Ensure the profile's on-disk file sits at the conventional
    /// `agents_dir/<name>.json` path.
    ///
    /// A profile's identity is its `name` FIELD, not its file name, so
    /// profiles occasionally live in oddly-named files (e.g. `general.json`
    /// holding `"name": "generalist"`). When `actual` differs from the
    /// conventional path, it is snapshotted first (F4), renamed to the
    /// conventional path, and the edit is left to touch a single predictable
    /// file with a restorable pre-edit state.
    ///
    /// Returns the path `edit_agent`/`revert_agent` operate on. No-op (and a
    /// plain `Ok` of the conventional path) when the file is already there or
    /// does not exist.
    pub fn canonicalize_profile_file(
        &self,
        name: &str,
        actual: &Path,
    ) -> Result<PathBuf, crate::agents::AgentError> {
        let conventional = self.agents_dir.join(format!("{}.json", name));
        if actual == conventional.as_path() || !actual.is_file() {
            return Ok(conventional);
        }
        self.snapshot_file(name, actual);
        std::fs::create_dir_all(&self.agents_dir).map_err(|e| {
            crate::agents::AgentError::ConfigError(format!(
                "Failed to create agents directory {:?}: {}",
                self.agents_dir, e
            ))
        })?;
        if conventional.exists() {
            // A conventional file for the same profile name already exists
            // (duplicate profile). Preserve it in history, then let the
            // renamed file become the sole canonical copy.
            self.snapshot_file(name, &conventional);
            std::fs::remove_file(&conventional).map_err(|e| {
                crate::agents::AgentError::ConfigError(format!(
                    "Failed to remove duplicate profile file {:?}: {}",
                    conventional, e
                ))
            })?;
        }
        std::fs::rename(actual, &conventional).map_err(|e| {
            crate::agents::AgentError::ConfigError(format!(
                "Failed to rename profile file {:?} to {:?}: {}",
                actual, conventional, e
            ))
        })?;
        tracing::info!(
            "Canonicalized agent profile '{}' to {}",
            name,
            conventional.display()
        );
        Ok(conventional)
    }

    /// Choose the destination path for the next snapshot of `name` at
    /// `now`. The first snapshot at a timestamp uses the base name
    /// (`<name>-<now>.json`, seq 0); every later one appends
    /// `-1`, `-2`, ... where the seq is (max existing seq for this
    /// timestamp) + 1 — NOT the first free slot. A reused low seq (e.g. the
    /// base name after it was pruned) would sort as the OLDEST snapshot and
    /// get pruned immediately.
    fn next_snapshot_path(&self, name: &str, now: u64) -> PathBuf {
        let hist = self.history_dir();
        let base = hist.join(format!("{}-{}.json", name, now));
        let prefix = format!("{}-{}-", name, now);
        let mut max_seq: u32 = 0;
        let mut any = false;
        if base.exists() {
            any = true;
        }
        if let Ok(entries) = std::fs::read_dir(&hist) {
            for entry in entries.flatten() {
                let file_name_os = entry.file_name();
                let Some(file_name) = file_name_os.to_str() else {
                    continue;
                };
                let Some(stem) = file_name.strip_suffix(".json") else {
                    continue;
                };
                let Some(tail) = stem.strip_prefix(&prefix) else {
                    continue;
                };
                if let Ok(seq) = tail.parse::<u32>() {
                    any = true;
                    if seq > max_seq {
                        max_seq = seq;
                    }
                }
            }
        }
        if !any {
            return base;
        }
        hist.join(format!("{}-{}-{}.json", name, now, max_seq + 1))
    }

    /// Delete the oldest snapshots of `name` beyond
    /// [`Self::HISTORY_SNAPSHOTS_KEEP`].
    fn prune_history(&self, name: &str) {
        let hist = self.history_dir();
        let prefix = format!("{}-", name);
        let entries = match std::fs::read_dir(&hist) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut snaps: Vec<(PathBuf, u64, u32)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|e| e == "json").unwrap_or(false) != true {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(".json") else {
                continue;
            };
            let Some(tail) = stem.strip_prefix(&prefix) else {
                continue;
            };
            if tail.is_empty() {
                continue;
            }
            let (ts, seq) = Self::history_file_order(tail);
            if ts > 0 {
                snaps.push((path, ts, seq));
            }
        }
        if snaps.len() <= Self::HISTORY_SNAPSHOTS_KEEP {
            return;
        }
        snaps.sort_by_key(|(path, ts, seq)| {
            (
                *ts,
                *seq,
                path.file_name()
                    .map(|f| f.to_os_string())
                    .unwrap_or_default(),
            )
        });
        let excess = snaps.len() - Self::HISTORY_SNAPSHOTS_KEEP;
        for (path, _, _) in snaps.into_iter().take(excess) {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("Failed to prune agent history {:?}: {}", path, e);
            }
        }
    }

    /// List the prompt-history snapshots of an agent, NEWEST first.
    ///
    /// Returns an empty vec when the agent has no history (missing history
    /// dir or no snapshots) — not an error.
    pub fn list_agent_history(
        &self,
        name: &str,
    ) -> Result<Vec<PathBuf>, crate::agents::AgentError> {
        let hist = self.history_dir();
        if !hist.is_dir() {
            return Ok(Vec::new());
        }
        let prefix = format!("{}-", name);
        let mut snaps: Vec<(PathBuf, u64, u32)> = Vec::new();
        let entries = std::fs::read_dir(&hist).map_err(|e| {
            crate::agents::AgentError::ConfigError(format!(
                "Failed to read history directory {:?}: {}",
                hist, e
            ))
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|e| e == "json").unwrap_or(false) != true {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(".json") else {
                continue;
            };
            let Some(tail) = stem.strip_prefix(&prefix) else {
                continue;
            };
            if tail.is_empty() {
                continue;
            }
            let (ts, seq) = Self::history_file_order(tail);
            if ts > 0 {
                snaps.push((path, ts, seq));
            }
        }
        snaps.sort_by_key(|(path, ts, seq)| {
            (
                *ts,
                *seq,
                path.file_name()
                    .map(|f| f.to_os_string())
                    .unwrap_or_default(),
            )
        });
        snaps.reverse(); // newest first
        Ok(snaps.into_iter().map(|(path, _, _)| path).collect())
    }

    /// Restore an agent from one of its history snapshots.
    ///
    /// `snapshot` must live inside [`Self::history_dir`] and name the agent
    /// (`<name>-<unixts>[-seq].json`). The CURRENT `agents_dir/<name>.json`
    /// is itself snapshotted first, so a revert is reversible. Returns the
    /// restored [`AgentConfig`] (with `agents_dir` anchored to this manager).
    pub fn revert_agent(
        &self,
        name: &str,
        snapshot: &Path,
    ) -> Result<AgentConfig, crate::agents::AgentError> {
        let hist = self.history_dir();
        // The snapshot must be a direct child of the history dir.
        if snapshot.parent() != Some(hist.as_path()) {
            return Err(crate::agents::AgentError::ConfigError(format!(
                "History snapshot {:?} is not under the history directory {:?}",
                snapshot, hist
            )));
        }
        let file_name = snapshot
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default();
        let Some(stem) = file_name.strip_suffix(".json") else {
            return Err(crate::agents::AgentError::ConfigError(format!(
                "History snapshot {:?} must be a .json file",
                snapshot
            )));
        };
        let Some(rest) = stem.strip_prefix(name).and_then(|s| s.strip_prefix('-')) else {
            return Err(crate::agents::AgentError::ConfigError(format!(
                "History snapshot {:?} does not belong to agent '{}'",
                snapshot, name
            )));
        };
        // Validate the `<unixts>[-seq]` suffix.
        let mut ts_ok = false;
        for part in rest.split('-') {
            if part.is_empty() || part.parse::<u64>().is_err() {
                ts_ok = false;
                break;
            }
            ts_ok = true;
        }
        if rest.is_empty() || !ts_ok {
            return Err(crate::agents::AgentError::ConfigError(format!(
                "History snapshot {:?} is not a valid history file for agent '{}'",
                snapshot, name
            )));
        }
        if !snapshot.is_file() {
            return Err(crate::agents::AgentError::ConfigError(format!(
                "History snapshot {:?} does not exist",
                snapshot
            )));
        }
        // Reverting is itself a config change: snapshot the current file so
        // the user can go forward again.
        self.snapshot_agent(name);
        let dst = self.agents_dir.join(format!("{}.json", name));
        std::fs::copy(snapshot, &dst).map_err(|e| {
            crate::agents::AgentError::ConfigError(format!(
                "Failed to revert agent '{}' from {:?}: {}",
                name, snapshot, e
            ))
        })?;
        let mut config: AgentConfig = Self::load_agent_file(&dst)?;
        config.agents_dir = self.agents_dir.clone();
        tracing::info!("Reverted agent '{}' from snapshot {:?}", name, snapshot);
        Ok(config)
    }

    /// Parse a single agent JSON file (new `AgentConfig` format first,
    /// legacy `WorkerConfig` fallback — same migration as discovery).
    fn load_agent_file(path: &Path) -> Result<AgentConfig, crate::agents::AgentError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            crate::agents::AgentError::ConfigError(format!(
                "Failed to read agent config {:?}: {}",
                path, e
            ))
        })?;
        if let Ok(config) = serde_json::from_str::<AgentConfig>(&content) {
            return Ok(config);
        }
        let legacy: WorkerConfig = serde_json::from_str(&content).map_err(|e| {
            crate::agents::AgentError::ConfigError(format!(
                "Failed to parse agent config {:?}: {}",
                path, e
            ))
        })?;
        Ok(AgentConfig {
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
            agents_dir: PathBuf::new(),
            agents_search_dirs: Vec::new(),
            custom_prompts: HashMap::new(),
            reasoning_effort: legacy.reasoning_effort,
            trim_config: crate::trimming::config::TrimConfig::default(),
            handoff_enabled: legacy.handoff_enabled,
            handoff_targets: legacy.can_invoke,
            restart_enabled: true,
        })
    }

    /// Reload all agent configs from disk (use after add/edit/remove).
    pub fn reload(&self) -> Result<Vec<AgentConfig>, crate::agents::AgentError> {
        let agents = self.list_agents();
        tracing::info!(
            "Reloaded {} agent config(s) from {:?}",
            agents.as_ref().map(|a| a.len()).unwrap_or(0),
            self.agents_dir
        );
        agents
    }
}

#[cfg(test)]
mod tests;
