use super::*;
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
    assert!(cfg.streaming);
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
    cfg.streaming = false;
    cfg.theme = "light".to_string();
    cfg.chat_history = vec![
        ChatMessage { role: "user".to_string(), content: "Hello".to_string(), timestamp: String::new() },
        ChatMessage { role: "assistant".to_string(), content: "Hi there!".to_string(), timestamp: String::new() },
    ];
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
    assert!(!loaded.streaming);
    assert_eq!(loaded.theme, "light");
    assert_eq!(loaded.chat_history.len(), 2);
    assert_eq!(loaded.chat_history[0].role, "user");
    assert_eq!(loaded.chat_history[0].content, "Hello");
    assert_eq!(loaded.chat_history[1].role, "assistant");
    assert_eq!(loaded.chat_history[1].content, "Hi there!");
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
    assert!(matches!(cfg.validate().unwrap_err(), Error::InvalidRemoteUrl(_)));
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
    cfg.streaming = false;
    cfg.theme = "light".to_string();
    cfg.chat_history = vec![
        ChatMessage { role: "user".to_string(), content: "Hello".to_string(), timestamp: String::new() },
        ChatMessage { role: "assistant".to_string(), content: "Hi!".to_string(), timestamp: String::new() },
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
    assert!(!loaded.streaming);
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
