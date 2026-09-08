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
