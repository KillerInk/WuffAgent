use std::path::PathBuf;

/// Returns the base application data directory: ~/.wuffagent
pub fn get_wuffagent_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".wuffagent")
}

/// Test override for [`get_config_path`] (tools that persist to the app
/// config file point it at a temp file). `OnceLock` so the override needs no
/// lock of its own; tests set/restore it around each test.
static TEST_CONFIG_PATH: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn test_config_path() -> &'static std::sync::Mutex<Option<PathBuf>> {
    TEST_CONFIG_PATH.get_or_init(|| std::sync::Mutex::new(None))
}

/// Set (or clear with `None`) the test override for the config file path.
pub fn set_config_path_for_testing(path: Option<PathBuf>) {
    *test_config_path().lock().unwrap() = path;
}

/// Returns the path to the config file (the test override wins when set).
pub fn get_config_path() -> PathBuf {
    if let Some(p) = test_config_path().lock().unwrap().clone() {
        return p;
    }
    get_wuffagent_home().join("config.json")
}

/// Test override for [`get_restart_marker_path`]; same pattern as
/// [`set_config_path_for_testing`]. Tests that use it serialize on a lock
/// (process-wide override).
static TEST_MARKER_PATH: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn test_marker_path() -> &'static std::sync::Mutex<Option<PathBuf>> {
    TEST_MARKER_PATH.get_or_init(|| std::sync::Mutex::new(None))
}

/// Set (or clear with `None`) the test override for the restart marker path.
pub fn set_restart_marker_path_for_testing(path: Option<PathBuf>) {
    *test_marker_path().lock().unwrap() = path;
}

/// Returns the path to the restart marker file, written by the UI just before
/// relaunching WuffAgent and read (then deleted) by the new process on startup
/// so it can auto-resume the session the agent was restarting for. The test
/// override wins when set.
pub fn get_restart_marker_path() -> PathBuf {
    if let Some(p) = test_marker_path().lock().unwrap().clone() {
        return p;
    }
    get_wuffagent_home().join("restart.json")
}

/// Read the restart marker, if present, and delete it (it fires exactly once).
///
/// Returns `None` when the marker is absent, unreadable, or not valid JSON.
/// A file that exists but cannot be parsed is LEFT in place for inspection —
/// deleting it would lose the restart reason without telling anyone why the
/// resume failed.
pub fn consume_restart_marker() -> Option<RestartMarker> {
    let path = get_restart_marker_path();
    let json = std::fs::read_to_string(&path).ok()?;
    let marker: RestartMarker = serde_json::from_str(&json).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(marker)
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
