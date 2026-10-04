use std::path::{Path, PathBuf};

/// Test override for the whole app-home directory (the test process's own
/// copy of `wuffagent-core` honors it — e.g. the fake-Telegram e2e test keeps
/// its `telegram.json`/state/log in a temp dir instead of touching the real
/// `~/.wuffagent`). `OnceLock` like the config-path override above; set/clear
/// around the test.
static TEST_HOME_DIR: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn test_home_dir() -> &'static std::sync::Mutex<Option<PathBuf>> {
    TEST_HOME_DIR.get_or_init(|| std::sync::Mutex::new(None))
}

/// Set (or clear with `None`) the test override for [`get_wuffagent_home`].
pub fn set_wuffagent_home_for_testing(path: Option<PathBuf>) {
    *test_home_dir().lock().unwrap() = path;
}

/// Returns the base application data directory: ~/.wuffagent
pub fn get_wuffagent_home() -> PathBuf {
    if let Some(p) = test_home_dir().lock().unwrap().clone() {
        return p;
    }
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

/// Copy freshly-built plugin files (`.dll` / `.so`) from `src_dir` into
/// `dst_dir` (the plugin discovery dir), overwriting same-named files. Used by
/// the UI right after a self-restart: the old process has already spawned this
/// one and closed, so the plugin DLLs are no longer file-locked — the only
/// window in which the installed copies can be overwritten. Creates `dst_dir`
/// if it does not exist. Returns one entry per candidate file — `Ok(installed_path)`
/// or `Err(message)` (a single failure never aborts the rest). An unreadable or
/// absent `src_dir` yields an empty list (nothing to install); non-plugin files
/// are never touched.
pub fn install_built_plugins(src_dir: &Path, dst_dir: &Path) -> Vec<Result<PathBuf, String>> {
    let Ok(entries) = std::fs::read_dir(src_dir) else {
        return Vec::new();
    };
    // A copy into a missing destination fails; create it once up front.
    let _ = std::fs::create_dir_all(dst_dir);
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("dll") && ext != Some("so") {
            continue;
        }
        let dest = dst_dir.join(path.file_name().unwrap_or_default());
        match std::fs::copy(&path, &dest) {
            Ok(_) => out.push(Ok(dest)),
            Err(e) => out.push(Err(format!(
                "copy '{}' -> '{}': {}",
                path.display(),
                dest.display(),
                e
            ))),
        }
    }
    out
}

#[cfg(test)]
mod install_tests {
    use super::install_built_plugins;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wa-install-plugins-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installs_dll_and_so_overwriting_existing() {
        let src = tmp_dir("src");
        let dst = tmp_dir("dst");
        std::fs::write(src.join("telegram_plugin.dll"), b"new-telegram").unwrap();
        std::fs::write(src.join("hello_plugin.so"), b"new-hello").unwrap();
        std::fs::write(src.join("notes.txt"), b"not a plugin").unwrap();
        // A stale installed file to be overwritten, plus one that must survive.
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("telegram_plugin.dll"), b"old-telegram").unwrap();
        std::fs::write(dst.join("other_plugin.dll"), b"keep-me").unwrap();

        let results = install_built_plugins(&src, &dst);
        let mut ok_names: Vec<String> = results
            .iter()
            .filter_map(|r| {
                r.as_ref()
                    .ok()
                    .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            })
            .collect();
        ok_names.sort();
        assert_eq!(
            ok_names,
            vec!["hello_plugin.so".to_string(), "telegram_plugin.dll".to_string()]
        );

        // The built version replaced the old one.
        assert_eq!(
            std::fs::read(dst.join("telegram_plugin.dll")).unwrap(),
            b"new-telegram"
        );
        // A plugin not present in the build dir is left untouched.
        assert_eq!(std::fs::read(dst.join("other_plugin.dll")).unwrap(), b"keep-me");
        // Non-plugin files are never copied.
        assert!(!dst.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn missing_src_dir_is_a_noop() {
        let dst = tmp_dir("dst-missing");
        let results = install_built_plugins(&dst.join("does-not-exist"), &dst);
        assert!(results.is_empty());
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn creates_missing_dst_dir() {
        let src = tmp_dir("src-create");
        let dst = src.join("new-plugins"); // does not exist yet
        std::fs::write(src.join("p.dll"), b"x").unwrap();
        let results = install_built_plugins(&src, &dst);
        assert_eq!(results.len(), 1);
        assert!(dst.join("p.dll").exists());
        let _ = std::fs::remove_dir_all(&src);
    }
}
