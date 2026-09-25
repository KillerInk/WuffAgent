//! MCP (Model Context Protocol) server management tools (T2): `mcp_list`,
//! `mcp_add_server`, `mcp_connect`, `mcp_disconnect`, `mcp_remove_server`,
//! `mcp_refresh_tools`, `mcp_set_tool_enabled`.
//!
//! Both are SHARED-REGISTRY tools: registered once via
//! [`super::register_mcp_tools`] with the app's [`McpManager`], and gated per
//! profile through `allowed_tools` like any other tool. (The `mcp__<server>__<tool>`
//! tools mirrored into the registry are the server's tools; these are the
//! management surface on top of them.)
//!
//! Concurrency: the `*_sync` McpManager operations `block_on` the manager's
//! dedicated runtime, so they must NOT run on a thread inside a runtime
//! context. `ToolManager` executes every tool on `spawn_blocking` workers
//! (plain threads, no runtime context), so calling them straight from
//! `Tool::execute` is safe.
//!
//! Persistence: `mcp_add_server` / `mcp_remove_server` mutate the LIVE manager
//! state AND re-serialize the whole app `config.json` (`mcp_servers` array),
//! the same way the UI's MCP panel does. `config.json` is owned by the app
//! process (the UI's `Config` handle is never reloaded mid-run), so the
//! read-modify-write is best-effort; a failure is reported but the live state
//! change stands.
//!
//! Layout: the shared helpers below (config path, atomic config write,
//! config read-modify-write, `run_mcp_op` timeout wrapper, status/error
//! mapping, UI event notification, and server-config parsing) are used by
//! both tool submodules and the unit tests, so they live here and are pulled
//! in by the submodules via `use super::*;`. The tool implementations are
//! split by concern:
//! - [`servers`]   — server lifecycle: `mcp_add_server` / `mcp_connect` /
//!                   `mcp_disconnect` / `mcp_remove_server`
//! - [`tool_mgmt`] — introspection + per-tool registration: `mcp_list` /
//!                   `mcp_refresh_tools` / `mcp_set_tool_enabled`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::config::{Config, McpServerConfig, McpTransport};
use crate::tools::mcp::{McpManager, McpServerStatus};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};
use crate::types::AppEvent;

pub mod servers;
pub mod tool_mgmt;

pub use servers::{McpAddServerTool, McpConnectTool, McpDisconnectTool, McpRemoveServerTool};
pub use tool_mgmt::{McpListTool, McpRefreshToolsTool, McpSetToolEnabledTool};

/// The MCP management tool names (for docs / allowlist discussions).
pub const MCP_TOOL_NAMES: &[&str] = &[
    "mcp_list",
    "mcp_add_server",
    "mcp_connect",
    "mcp_disconnect",
    "mcp_remove_server",
    "mcp_refresh_tools",
    "mcp_set_tool_enabled",
];

/// The app config file. `get_config_path()` is deterministic (`~/.wuffagent/
/// config.json`), and the in-memory `file_path` field always resolves to the
/// same location — but re-deriving it keeps the tool independent of whatever
/// the UI's `Config` handle currently thinks.
fn config_path() -> PathBuf {
    crate::config::get_config_path()
}

/// Serialize a full `Config` to `path` via a temp file + rename (atomic, so a
/// crash mid-write cannot corrupt the config — same discipline as
/// `memory/storage.rs`).
fn atomic_write_config(config: &Config, path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
    }
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("failed to serialize config: {}", e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &content)
        .map_err(|e| format!("failed to write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("failed to move {} to {}: {}", tmp.display(), path.display(), e))?;
    Ok(())
}

/// Read-modify-write the `mcp_servers` array in the app config file.
/// `update` sees the current list (cloned out before writing) and returns the
/// new one. Returns the new list plus any persistence warning (live manager
/// state is authoritative either way).
fn update_mcp_servers_in_config(
    update: impl FnOnce(&[McpServerConfig]) -> Vec<McpServerConfig>,
) -> (Vec<McpServerConfig>, Option<String>) {
    let path = config_path();
    let mut cfg = match Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("mcp config update: failed to load {}: {} (using defaults)", path.display(), e);
            Config {
                file_path: path.clone(),
                ..Default::default()
            }
        }
    };
    let updated = update(&cfg.mcp_servers);
    cfg.mcp_servers = updated.clone();
    match atomic_write_config(&cfg, &path) {
        Ok(()) => (updated, None),
        Err(e) => (
            updated,
            Some(format!(
                "Config file write failed ({}), the change is live for this run but will not survive a restart",
                e
            )),
        ),
    }
}

/// Run a blocking `McpManager` op with a hard wall-clock ceiling, so a wedged
/// transport cannot hold the agent's tool loop forever (the MCP client has its
/// own per-request timeouts; this is a backstop, and mirrors the UI panel's
/// per-op timeouts). The op's thread is abandoned on timeout (detached — it
/// will finish on its own).
fn run_mcp_op<T: Send + 'static>(
    label: &str,
    timeout: std::time::Duration,
    op: impl FnOnce() -> Result<T, crate::tools::mcp::McpError> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("mcp-tool-op".to_string())
        .spawn(move || {
            let result = op();
            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to start MCP worker thread for {}: {}", label, e))?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!(
            "{} timed out after {}s (the operation may still be finishing in the background)",
            label,
            timeout.as_secs()
        )),
    }
}

fn status_str(status: &McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Connecting => "connecting",
        McpServerStatus::Connected { .. } => "connected",
        McpServerStatus::Error(_) => "error",
    }
}

fn mcp_err(e: crate::tools::mcp::McpError) -> ToolError {
    ToolError::Execution(e.to_string())
}

/// Emit `AppEvent::McpConfigChanged` to the UI event channel, telling the UI
/// that its in-memory `config.mcp_servers` list is stale and must be
/// reloaded from disk. `session_id` is `None` when the change did not come
/// from an agent run (e.g. the UI's own MCP panel — in which case the UI
/// already has the updated list, but the event is still harmless).
fn notify_config_changed(
    events: &Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    session_id: Option<&str>,
) {
    let Some(tx) = events else {
        return;
    };
    let sid = session_id
        .map(|s| s.to_string())
        .unwrap_or_default();
    let _ = tx.lock().unwrap().send(AppEvent::McpConfigChanged {
        session_id: sid,
    });
}

/// Build an `McpServerConfig` from the loose tool parameters.
fn parse_server_config(params: &ToolParams) -> Result<McpServerConfig, String> {
    let name: String = params
        .get("name")
        .ok_or_else(|| "name is required".to_string())?;
    let name = name.trim().to_string();

    let transport: McpTransport = match params.get::<String>("transport").as_deref() {
        Some("http") => {
            let url: String = params
                .get("url")
                .ok_or_else(|| "url is required for the http transport".to_string())?;
            let headers: HashMap<String, String> = params.get("headers").unwrap_or_default();
            McpTransport::Http { url, headers }
        }
        Some("stdio") | Some("command") | None => {
            let command: String = params
                .get("command")
                .ok_or_else(|| "command is required for the stdio transport".to_string())?;
            let args: Vec<String> = params.get("args").unwrap_or_default();
            let env: HashMap<String, String> = params.get("env").unwrap_or_default();
            let working_dir: Option<String> = params.get("working_dir");
            McpTransport::Stdio {
                command,
                args,
                env,
                working_dir,
            }
        }
        Some(other) => {
            return Err(format!(
                "transport must be 'stdio' or 'http' (got '{}')",
                other
            ))
        }
    };

    let enabled: bool = params.get("enabled").unwrap_or(true);
    let timeout_secs: u64 = params.get("timeout_secs").unwrap_or(60);
    let allowed_tools: Vec<String> = params.get("allowed_tools").unwrap_or_default();

    Ok(McpServerConfig {
        name,
        transport,
        enabled,
        timeout_secs,
        allowed_tools,
    })
}

#[cfg(test)]
mod tests;
