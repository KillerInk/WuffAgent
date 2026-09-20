use std::path::PathBuf;

/// Returns the base application data directory: ~/.wuffagent
pub fn get_wuffagent_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".wuffagent")
}

/// Returns the path to the config file.
pub fn get_config_path() -> PathBuf {
    get_wuffagent_home().join("config.json")
}

/// Returns the path to the restart marker file, written by the UI just before
/// relaunching WuffAgent and read (then deleted) by the new process on startup
/// so it can auto-resume the session the agent was restarting for.
pub fn get_restart_marker_path() -> PathBuf {
    get_wuffagent_home().join("restart.json")
}

/// The marker persisted across a restart so the new process can resume the
/// session automatically (written by the UI's `perform_restart`, consumed once
/// in `main` after bootstrap).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct RestartMarker {
    /// The session to resume (set as the config's active session on startup).
    pub session_id: String,
    /// Why the agent restarted (used to build the auto-resume turn).
    pub reason: String,
}
