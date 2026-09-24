//! Tests for the MCP management tools (T2).
//!
//! Pure-logic coverage: config parsing/validation and the config-file
//! read-modify-write. The blocking ops (`connect_sync` etc.) are exercised
//! end-to-end by the existing manager integration tests in `tools/mcp/mod.rs`
//! (python mock server).

use std::collections::HashMap;

use super::*;
use crate::tools::types::ToolParams;

/// A temp config file (removed first if it exists).
fn temp_config(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "wuffagent_mcp_tool_test_{}_{}.json",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Point `get_config_path()` at a temp file for the duration of the test.
/// Tests touching the config file run sequentially (a single test binary's
/// thread pool is the only concurrency we rely on here).
struct ConfigPathGuard {
    previous: Option<PathBuf>,
}

impl ConfigPathGuard {
    fn new(path: PathBuf) -> Self {
        let previous = crate::config::get_config_path();
        let previous = if previous == crate::config::get_wuffagent_home().join("config.json") {
            None
        } else {
            Some(previous)
        };
        crate::config::set_config_path_for_testing(Some(path));
        ConfigPathGuard { previous }
    }
}

impl Drop for ConfigPathGuard {
    fn drop(&mut self) {
        crate::config::set_config_path_for_testing(self.previous.clone());
    }
}

fn tool_params(json: serde_json::Value) -> ToolParams {
    let values = json
        .as_object()
        .expect("params must be an object")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    ToolParams { values }
}

fn stdio_cfg(name: &str, command: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransport::Stdio {
            command: command.to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            working_dir: None,
        },
        enabled: true,
        timeout_secs: 60,
        allowed_tools: Vec::new(),
    }
}

#[test]
fn test_parse_server_config_stdio_defaults() {
    let params = tool_params(serde_json::json!({
        "name": "srv",
        "command": "npx",
        "args": ["-y", "server"]
    }));
    let cfg = parse_server_config(&params).unwrap();
    assert_eq!(cfg.name, "srv");
    match cfg.transport {
        McpTransport::Stdio {
            command,
            args,
            working_dir,
            ..
        } => {
            assert_eq!(command, "npx");
            assert_eq!(args, vec!["-y".to_string(), "server".to_string()]);
            assert_eq!(working_dir, None);
        }
        _ => panic!("expected stdio"),
    }
    assert!(cfg.enabled, "enabled defaults to true");
    assert_eq!(cfg.timeout_secs, 60, "timeout defaults to 60");
    assert!(cfg.allowed_tools.is_empty());
}

#[test]
fn test_parse_server_config_http() {
    let params = tool_params(serde_json::json!({
        "name": "web",
        "transport": "http",
        "url": "http://127.0.0.1:9/mcp",
        "headers": { "Authorization": "Bearer x" }
    }));
    let cfg = parse_server_config(&params).unwrap();
    match cfg.transport {
        McpTransport::Http { url, headers } => {
            assert_eq!(url, "http://127.0.0.1:9/mcp");
            assert_eq!(headers.get("Authorization").map(String::as_str), Some("Bearer x"));
        }
        _ => panic!("expected http"),
    }
}

#[test]
fn test_parse_server_config_rejects_missing_required() {
    // No transport hint and no command → stdio requires command.
    let params = tool_params(serde_json::json!({ "name": "x" }));
    let err = parse_server_config(&params).unwrap_err();
    assert!(err.contains("command"), "got: {}", err);

    // Explicit http without url.
    let params = tool_params(serde_json::json!({ "name": "x", "transport": "http" }));
    let err = parse_server_config(&params).unwrap_err();
    assert!(err.contains("url"), "got: {}", err);

    // Unknown transport.
    let params = tool_params(serde_json::json!({
        "name": "x",
        "transport": "carrier-pigeon"
    }));
    let err = parse_server_config(&params).unwrap_err();
    assert!(err.contains("transport"), "got: {}", err);

    // No name.
    let params = tool_params(serde_json::json!({ "command": "npx" }));
    let err = parse_server_config(&params).unwrap_err();
    assert!(err.contains("name"), "got: {}", err);
}

#[test]
fn test_mcp_add_server_validation_errors() {
    // Bad name is rejected by `McpServerConfig::validate` before any connect
    // (connect_now=false would also keep this test from spawning anything).
    let params = tool_params(serde_json::json!({
        "name": "bad name!",
        "command": "npx",
        "connect_now": false
    }));
    let cfg = parse_server_config(&params).unwrap();
    let err = cfg.validate(&[]).unwrap_err();
    assert!(err.contains("letters"), "got: {}", err);
}

#[test]
fn test_update_mcp_servers_add_and_remove_roundtrip() {
    let path = temp_config("rmw");
    let _guard = ConfigPathGuard::new(path.clone());

    // No config file yet → starts from defaults.
    let (list, warn) = update_mcp_servers_in_config(|current| {
        assert!(current.is_empty());
        let mut list = current.to_vec();
        list.push(stdio_cfg("a", "npx-a"));
        list.push(stdio_cfg("b", "npx-b"));
        list
    });
    assert!(warn.is_none());
    assert_eq!(list.len(), 2);

    // The file now holds the array; a second update sees it.
    let (list, warn) = update_mcp_servers_in_config(|current| {
        let names: Vec<String> = current.iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
        current.iter().filter(|c| c.name != "a").cloned().collect()
    });
    assert!(warn.is_none());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "b");

    // The on-disk file parses as a full Config with the right array.
    let on_disk: Config = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk.mcp_servers.len(), 1);
    assert_eq!(on_disk.mcp_servers[0].name, "b");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_atomic_write_config_preserves_unrelated_fields() {
    let path = temp_config("atomic");
    let _guard = ConfigPathGuard::new(path.clone());
    let mut cfg = Config::default();
    cfg.file_path = path.clone();
    cfg.system_prompt = "keep me".to_string();
    cfg.n_ctx = 4096;
    cfg.mcp_servers = vec![stdio_cfg("one", "cmd-one")];
    cfg.save().unwrap();

    // Re-load, change only the mcp array, save again.
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.system_prompt, "keep me");
    let mut updated = loaded;
    updated.mcp_servers = vec![stdio_cfg("two", "cmd-two")];
    atomic_write_config(&updated, &path).unwrap();

    let final_cfg = Config::load(&path).unwrap();
    assert_eq!(final_cfg.system_prompt, "keep me", "unrelated field must survive");
    assert_eq!(final_cfg.n_ctx, 4096);
    assert_eq!(final_cfg.mcp_servers.len(), 1);
    assert_eq!(final_cfg.mcp_servers[0].name, "two");
    // No temp file left behind.
    assert!(!path.with_extension("json.tmp").exists());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_run_mcp_op_succeeds_and_errors() {
    let ok = run_mcp_op("op", std::time::Duration::from_secs(5), || Ok(42usize));
    assert_eq!(ok.unwrap(), 42);

    let err = run_mcp_op::<usize>(
        "op",
        std::time::Duration::from_secs(5),
        || Err(crate::tools::mcp::McpError::Other("boom".to_string())),
    );
    assert!(err.unwrap_err().contains("boom"));
}
