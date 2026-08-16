use std::path::PathBuf;

/// Returns the path to the config file.
pub fn get_config_path() -> PathBuf {
    // Try executable directory first (portable app style)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("config.json");
            return path;
        }
    }

    // Fallback: platform config directory
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("config.json")
}
