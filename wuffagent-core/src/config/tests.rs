// Tests for config module
use crate::config::{ChatMessage, Config, ConnectionType, Error};
use tempfile::tempdir;

#[test]
fn test_config_default() {
    let cfg = Config::default();
    assert_eq!(cfg.port, 8080);
    assert_eq!(cfg.n_gpu_layers, 99);
    assert_eq!(cfg.n_ctx, 4096);
    assert_eq!(cfg.threads, 8);
    assert_eq!(cfg.theme, "dark");
    assert_eq!(cfg.connection_type, ConnectionType::Local);
    assert!(cfg.server_path.is_empty());
    assert!(cfg.model_path.is_empty());
    assert!(cfg.chat_history.is_empty());
    assert_eq!(cfg.remote_url, "");
}

#[test]
fn test_config_load_nonexistent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = Config::load(&path).unwrap();
    assert_eq!(cfg.port, 8080);
    assert_eq!(cfg.connection_type, ConnectionType::Local);
    assert!(cfg.server_path.is_empty());
}

#[test]
fn test_config_save_and_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");

    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = "C:\\llama\\llama-server.exe".to_string();
    cfg.model_path = "C:\\models\\model.gguf".to_string();
    cfg.port = 18080;
    cfg.threads = 4;
    cfg.n_gpu_layers = 33;
    cfg.n_ctx = 2048;
    cfg.system_prompt = "You are a helpful assistant.".to_string();
    cfg.theme = "light".to_string();
    cfg.chat_history = vec![
        ChatMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
            timestamp: String::new(),
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: "Hi there!".to_string(),
            timestamp: String::new(),
        },
    ];
    cfg.reasoning_effort = crate::types::ReasoningEffort::High;
    cfg.file_path = path.clone();

    cfg.save().unwrap();

    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.connection_type, ConnectionType::Local);
    assert_eq!(loaded.server_path, "C:\\llama\\llama-server.exe");
    assert_eq!(loaded.model_path, "C:\\models\\model.gguf");
    assert_eq!(loaded.port, 18080);
    assert_eq!(loaded.threads, 4);
    assert_eq!(loaded.n_gpu_layers, 33);
    assert_eq!(loaded.n_ctx, 2048);
    assert_eq!(loaded.system_prompt, "You are a helpful assistant.");
    assert_eq!(loaded.theme, "light");
    assert_eq!(loaded.chat_history.len(), 2);
    assert_eq!(loaded.chat_history[0].role, "user");
    assert_eq!(loaded.chat_history[0].content, "Hello");
    assert_eq!(loaded.chat_history[1].role, "assistant");
    assert_eq!(loaded.chat_history[1].content, "Hi there!");
    assert_eq!(loaded.reasoning_effort, crate::types::ReasoningEffort::High);
}

#[test]
fn test_config_reasoning_effort_defaults_to_off_when_missing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");
    // Valid config JSON without a reasoning_effort key (pre-existing file).
    std::fs::write(
        &path,
        r#"{"server_path":"s","model_path":"m","port":8080,"n_gpu_layers":99,"n_ctx":4096,"threads":8,"remote_url":"","system_prompt":"","streaming":true,"theme":"dark","chat_history":[]}"#,
    )
    .unwrap();
    let cfg = Config::load(&path).unwrap();
    assert_eq!(cfg.reasoning_effort, crate::types::ReasoningEffort::Off);
}

#[test]
fn test_config_validate_empty_paths() {
    let cfg = Config::default();
    // Local mode with empty server_path should fail
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_nonexistent_paths() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = "/nonexistent/server".to_string();
    cfg.model_path = "/nonexistent/model".to_string();
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_invalid_port() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = "/tmp/server".to_string();
    cfg.model_path = "/tmp/model".to_string();
    cfg.port = 80; // below 1024
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_invalid_threads() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = "/tmp/server".to_string();
    cfg.model_path = "/tmp/model".to_string();
    cfg.threads = 0;
    assert!(cfg.validate().is_err());

    cfg.threads = 65;
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_invalid_gpu_layers() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = "/tmp/server".to_string();
    cfg.model_path = "/tmp/model".to_string();
    cfg.n_gpu_layers = -1;
    assert!(cfg.validate().is_err());

    cfg.n_gpu_layers = 100;
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_valid() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = "/tmp/server".to_string();
    cfg.model_path = "/tmp/model".to_string();
    cfg.port = 8080;
    cfg.threads = 4;
    cfg.n_gpu_layers = 33;
    // Create temp files so paths exist
    let dir = tempdir().unwrap();
    let server_file = dir.path().join("server");
    let model_file = dir.path().join("model");
    std::fs::write(&server_file, "").unwrap();
    std::fs::write(&model_file, "").unwrap();
    cfg.server_path = server_file.to_string_lossy().to_string();
    cfg.model_path = model_file.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_remote_mode_validation() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Remote;
    cfg.remote_url = "http://192.168.1.100:8080".to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_remote_mode_missing_url() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Remote;
    let err = cfg.validate().unwrap_err();
    assert!(matches!(err, Error::EmptyRemoteUrl));
}

#[test]
fn test_config_remote_mode_invalid_url() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Remote;
    cfg.remote_url = "ftp://bad-url".to_string();
    assert!(matches!(
        cfg.validate().unwrap_err(),
        Error::InvalidRemoteUrl(_)
    ));
}

#[test]
fn test_config_base_url_local() {
    let cfg = Config::default();
    assert_eq!(cfg.base_url(), "http://127.0.0.1:8080");
}

#[test]
fn test_config_base_url_remote() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Remote;
    cfg.remote_url = "http://example.com:3000".to_string();
    assert_eq!(cfg.base_url(), "http://example.com:3000");
}

#[test]
fn test_config_is_remote() {
    let cfg = Config::default();
    assert!(!cfg.is_remote());
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Remote;
    assert!(cfg.is_remote());
}

#[test]
fn test_config_backward_compat_deserialize() {
    let json = r#"{
        "server_path": "C:\\llama\\llama-server.exe",
        "model_path": "C:\\models\\model.gguf",
        "port": 18080,
        "n_gpu_layers": 33,
        "n_ctx": 2048,
        "threads": 4,
        "system_prompt": "test",
        "streaming": true,
        "theme": "dark",
        "chat_history": []
    }"#;
    let cfg: Config = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.connection_type, ConnectionType::Local);
    assert_eq!(cfg.server_path, "C:\\llama\\llama-server.exe");
    assert_eq!(cfg.port, 18080);
    assert!(cfg.remote_url.is_empty());
}

#[test]
fn test_connection_type_default() {
    assert_eq!(ConnectionType::default(), ConnectionType::Local);
}

// ── Port boundary tests ─────────────────────────────────────────────────────

#[test]
fn test_config_validate_port_min_boundary() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.port = 1;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_err());
}

#[test]
fn test_config_validate_port_max_boundary() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.port = 65535;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_port_zero() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.port = 0;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_err());
}

// ── Threads boundary tests ──────────────────────────────────────────────────

#[test]
fn test_config_validate_threads_min_boundary() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.threads = 1;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_threads_max_boundary() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.threads = 64;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_threads_65() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.threads = 65;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_err());
}

// ── GPU layers boundary tests ───────────────────────────────────────────────

#[test]
fn test_config_validate_gpu_layers_zero() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.n_gpu_layers = 0;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_gpu_layers_99() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.n_gpu_layers = 99;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_gpu_layers_100() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.n_gpu_layers = 100;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_err());
}

// ── Edge case tests ─────────────────────────────────────────────────────────

#[test]
fn test_config_validate_very_large_ctx() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.n_ctx = 100_000;
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    // n_ctx has no upper bound in validate(), so this should pass
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_empty_system_prompt() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.system_prompt = String::new();
    let dir = tempdir().unwrap();
    let server = dir.path().join("s");
    let model = dir.path().join("m");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn test_config_validate_whitespace_paths() {
    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    // Paths with whitespace that still point to existing files
    let dir = tempdir().unwrap();
    let server = dir.path().join("my server.exe");
    let model = dir.path().join("my model.gguf");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    assert!(cfg.validate().is_ok());
}

// ── Serialization edge-case tests ───────────────────────────────────────────

#[test]
fn test_config_serialize_deserialize_all_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");
    let server = dir.path().join("server");
    let model = dir.path().join("model");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&model, "").unwrap();

    let mut cfg = Config::default();
    cfg.connection_type = ConnectionType::Local;
    cfg.server_path = server.to_string_lossy().to_string();
    cfg.model_path = model.to_string_lossy().to_string();
    cfg.port = 18080;
    cfg.threads = 4;
    cfg.n_gpu_layers = 33;
    cfg.n_ctx = 2048;
    cfg.system_prompt = "You are a helpful agent.".to_string();
    cfg.theme = "light".to_string();
    cfg.chat_history = vec![
        ChatMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
            timestamp: String::new(),
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: "Hi!".to_string(),
            timestamp: String::new(),
        },
    ];
    cfg.remote_url = String::new();
    cfg.remote_api_key = Some("secret".to_string());
    cfg.encryption_enabled = true;
    cfg.encryption_password = Some("password".to_string());
    cfg.max_messages = 50;
    cfg.file_path = path.clone();

    cfg.save().unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.port, 18080);
    assert_eq!(loaded.threads, 4);
    assert_eq!(loaded.n_gpu_layers, 33);
    assert_eq!(loaded.n_ctx, 2048);
    assert_eq!(loaded.system_prompt, "You are a helpful agent.");
    assert_eq!(loaded.theme, "light");
    assert_eq!(loaded.chat_history.len(), 2);
    assert_eq!(loaded.max_messages, 50);
    assert_eq!(loaded.encryption_enabled, true);
    assert_eq!(loaded.encryption_password, Some("password".to_string()));
    assert_eq!(loaded.remote_api_key, Some("secret".to_string()));
}

#[test]
fn test_config_deserialize_unknown_fields_ignored() {
    let json = r#"{
        "server_path": "C:\\llama\\llama-server.exe",
        "model_path": "C:\\models\\model.gguf",
        "port": 18080,
        "n_gpu_layers": 33,
        "n_ctx": 2048,
        "threads": 4,
        "system_prompt": "test",
        "streaming": true,
        "theme": "dark",
        "chat_history": [],
        "unknown_field_one": 42,
        "unknown_field_two": "hello",
        "unknown_field_three": true
    }"#;
    let cfg: Config = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.port, 18080);
    assert_eq!(cfg.threads, 4);
    assert_eq!(cfg.n_gpu_layers, 33);
}

#[test]
fn test_config_old_file_without_mcp_servers_loads() {
    // A config file written before MCP support (no `mcp_servers` key at all)
    // must still load, with an empty server list.
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"server_path":"s","model_path":"m","port":8080,"n_gpu_layers":99,"n_ctx":4096,"threads":8,"remote_url":"","system_prompt":"","theme":"dark","chat_history":[]}"#,
    )
    .unwrap();
    let mut cfg = Config::load(&path).unwrap();
    assert!(cfg.mcp_servers.is_empty());
    // And it survives a save/reload roundtrip with the new field present.
    cfg.mcp_servers.push(crate::config::McpServerConfig {
        name: "fs".to_string(),
        transport: crate::config::McpTransport::Stdio {
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            env: std::collections::HashMap::new(),
            working_dir: None,
        },
        enabled: true,
        timeout_secs: 60,
        allowed_tools: Vec::new(),
    });
    cfg.file_path = path.clone();
    cfg.save().unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.mcp_servers.len(), 1);
    assert_eq!(loaded.mcp_servers[0].name, "fs");
    assert_eq!(loaded.mcp_servers[0].timeout_secs, 60);
}

// ─── Restart marker consumption (T4) ────────────────────────────────────────

/// The marker-path override is process-wide; serialize the tests that use it.
static MARKER_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn test_consume_restart_marker_present_reads_and_deletes() {
    let _g = MARKER_PATH_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let path = dir.path().join("restart.json");
    std::fs::write(
        &path,
        r#"{"session_id":"sess-1","reason":"rebuild after code change"}"#,
    )
    .unwrap();
    crate::config::set_restart_marker_path_for_testing(Some(path.clone()));
    let marker = crate::config::consume_restart_marker().expect("marker must be read");
    assert_eq!(marker.session_id, "sess-1");
    assert_eq!(marker.reason, "rebuild after code change");
    assert!(!path.exists(), "marker must be deleted after consumption");
    crate::config::set_restart_marker_path_for_testing(None);
}

#[test]
fn test_consume_restart_marker_corrupt_returns_none_keeps_file() {
    let _g = MARKER_PATH_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let path = dir.path().join("restart.json");
    std::fs::write(&path, "this is not json").unwrap();
    crate::config::set_restart_marker_path_for_testing(Some(path.clone()));
    assert!(
        crate::config::consume_restart_marker().is_none(),
        "corrupt marker must yield None"
    );
    assert!(
        path.exists(),
        "corrupt marker file must be kept for inspection"
    );
    crate::config::set_restart_marker_path_for_testing(None);
}

#[test]
fn test_consume_restart_marker_absent_returns_none() {
    let _g = MARKER_PATH_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let path = dir.path().join("restart.json"); // never created
    crate::config::set_restart_marker_path_for_testing(Some(path));
    assert!(crate::config::consume_restart_marker().is_none());
    crate::config::set_restart_marker_path_for_testing(None);
}

// ─── Presets path resolution (app home + legacy exe-dir migration) ──────────

/// Regression: presets used to live next to the executable, so they "vanished"
/// when the build dir changed (target/debug ↔ target/relaunch/debug). They now
/// live in the app home next to config.json, with one-time migration.
#[test]
fn test_resolve_presets_path_primary_exists_wins() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("presets.json");
    let legacy = dir.path().join("legacy-presets.json");
    std::fs::write(&primary, r#"{"presets":[]}"#).unwrap();
    std::fs::write(
        &legacy,
        r#"{"presets":[{"type":"remote","name":"old","remote_url":"http://x"}]}"#,
    )
    .unwrap();
    let resolved = crate::config::presets::resolve_presets_path(&primary, Some(&legacy));
    assert_eq!(resolved, primary);
    // The existing primary file must not be clobbered by the legacy one.
    assert_eq!(
        std::fs::read_to_string(&primary).unwrap(),
        r#"{"presets":[]}"#
    );
}

#[test]
fn test_resolve_presets_path_migrates_non_empty_legacy() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("presets.json"); // absent
    let legacy = dir.path().join("legacy-presets.json");
    let legacy_json =
        r#"{"presets":[{"type":"remote","name":"my","remote_url":"http://x"}]}"#;
    std::fs::write(&legacy, legacy_json).unwrap();
    let resolved = crate::config::presets::resolve_presets_path(&primary, Some(&legacy));
    assert_eq!(resolved, primary);
    assert!(
        primary.exists(),
        "legacy content must be migrated to the primary location"
    );
    assert_eq!(std::fs::read_to_string(&primary).unwrap(), legacy_json);
    assert!(legacy.exists(), "the legacy file is kept (copy, not move)");
}

#[test]
fn test_resolve_presets_path_empty_legacy_not_migrated() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("presets.json"); // absent
    let legacy = dir.path().join("legacy-presets.json");
    std::fs::write(&legacy, r#"{"presets": []}"#).unwrap();
    let resolved = crate::config::presets::resolve_presets_path(&primary, Some(&legacy));
    assert_eq!(resolved, primary);
    assert!(
        !primary.exists(),
        "an empty legacy store carries no data and must not shadow the primary"
    );
}

#[test]
fn test_resolve_presets_path_corrupt_legacy_not_migrated() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("presets.json"); // absent
    let legacy = dir.path().join("legacy-presets.json");
    std::fs::write(&legacy, "not json at all").unwrap();
    let resolved = crate::config::presets::resolve_presets_path(&primary, Some(&legacy));
    assert_eq!(resolved, primary);
    assert!(
        !primary.exists(),
        "invalid legacy content must not be copied (would break PresetStore::load)"
    );
}

#[test]
fn test_resolve_presets_path_no_files_returns_primary() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("presets.json"); // never created
    let resolved = crate::config::presets::resolve_presets_path(&primary, None);
    assert!(!primary.exists(), "no migration may create the file");
}

// ─── Preset::matches (the settings dialog's "(active)" marker) ──────────────

fn local_config() -> crate::config::Config {
    let mut cfg = crate::config::Config::default();
    cfg.connection_type = crate::config::ConnectionType::Local;
    cfg.server_path = "C:\\llama\\server.exe".to_string();
    cfg.model_path = "C:\\llama\\model.gguf".to_string();
    cfg.port = 8081;
    cfg.n_gpu_layers = 45;
    cfg.n_ctx = 8192;
    cfg.threads = 12;
    cfg
}

fn matching_local_preset() -> crate::config::Preset {
    crate::config::Preset::Local(crate::config::LocalPreset {
        name: "mine".to_string(),
        server_path: "C:\\llama\\server.exe".to_string(),
        model_path: "C:\\llama\\model.gguf".to_string(),
        port: 8081,
        n_gpu_layers: 45,
        n_ctx: 8192,
        threads: 12,
    })
}

#[test]
fn test_preset_matches_identical_local_config() {
    let cfg = local_config();
    assert!(matching_local_preset().matches(&cfg));
}

#[test]
fn test_preset_local_any_field_mismatch_is_inactive() {
    let cfg = local_config();
    let with = |f: &str| {
        let mut p = matching_local_preset();
        match p {
            crate::config::Preset::Local(ref mut lp) => match f {
                "server_path" => lp.server_path = "other".into(),
                "model_path" => lp.model_path = "other".into(),
                "port" => lp.port = 9999,
                "n_gpu_layers" => lp.n_gpu_layers = -1,
                "n_ctx" => lp.n_ctx = 256,
                "threads" => lp.threads = 1,
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
        p
    };
    // Each single-field drift must clear the active marker.
    for field in [
        "server_path",
        "model_path",
        "port",
        "n_gpu_layers",
        "n_ctx",
        "threads",
    ] {
        let p = with(field);
        assert!(!p.matches(&cfg), "{field} drift must clear the marker");
    }
}

#[test]
fn test_preset_type_mismatch_is_inactive() {
    let local_cfg = local_config();
    let remote_preset = crate::config::Preset::Remote(crate::config::RemotePreset {
        name: "remote".to_string(),
        remote_url: "http://x".to_string(),
        remote_api_key: Some("k".to_string()),
    });
    assert!(!remote_preset.matches(&local_cfg), "wrong connection type");

    let mut remote_cfg = crate::config::Config::default();
    remote_cfg.connection_type = crate::config::ConnectionType::Remote;
    remote_cfg.remote_url = "http://x".to_string();
    remote_cfg.remote_api_key = Some("k".to_string());
    assert!(remote_preset.matches(&remote_cfg));

    // API key Some vs None is a mismatch; None vs None matches.
    remote_cfg.remote_api_key = None;
    assert!(!remote_preset.matches(&remote_cfg));
    let no_key = crate::config::Preset::Remote(crate::config::RemotePreset {
        name: "remote".to_string(),
        remote_url: "http://x".to_string(),
        remote_api_key: None,
    });
    assert!(no_key.matches(&remote_cfg));

    let local_preset = matching_local_preset();
    assert!(!local_preset.matches(&remote_cfg));
}
