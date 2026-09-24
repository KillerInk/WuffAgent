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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::config::{Config, McpServerConfig, McpTransport};
use crate::tools::mcp::{McpManager, McpServerStatus};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};

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

// ─── mcp_list ─────────────────────────────────────────────────────────────────

/// Lists the configured MCP servers: live status, transport, and the
/// per-tool registry names (`mcp__<server>__<tool>`) an agent's
/// `allowed_tools` must use to see a tool.
pub struct McpListTool {
    manager: Arc<McpManager>,
}

impl McpListTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpListTool {
    fn name(&self) -> &str {
        "mcp_list"
    }

    fn description(&self) -> &str {
        "List configured MCP (Model Context Protocol) servers with their live \
         status (configured/connecting/connected/error), transport and the \
         tools each exposes. Server tools are registered in the tool registry \
         as `mcp__<server>__<tool>`; to use one from an agent profile, list the \
         full name in that profile's allowed_tools. Call mcp_add_server / \
         mcp_connect / mcp_disconnect / mcp_remove_server / mcp_refresh_tools / \
         mcp_set_tool_enabled to manage servers."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "mcp_list".to_string(),
            description: "List configured MCP servers and their tools".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: Vec::new(),
            }),
        }
    }

    fn execute(&self, _params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let servers = self.manager.snapshot();
        let profiles: Vec<serde_json::Value> = servers
            .iter()
            .map(|s| {
                let error = match &s.status {
                    McpServerStatus::Error(e) => Some(e.clone()),
                    _ => None,
                };
                let tools: Vec<serde_json::Value> = s
                    .tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "name": t.name,
                            "registry_name": t.full_name,
                            "description": t.description,
                            "enabled": t.enabled,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "name": s.name,
                    "transport": s.transport_summary,
                    "enabled": s.enabled,
                    "status": status_str(&s.status),
                    "error": error,
                    "timeout_secs": s.timeout_secs,
                    "tools": tools,
                })
            })
            .collect();
        Ok(ToolOutput::Success(serde_json::json!({
            "servers": profiles,
            "note": "Tools of connected servers are available to agents under the \
                     registry_name (subject to the agent profile's allowed_tools).",
        })))
    }
}

// ─── mcp_add_server ───────────────────────────────────────────────────────────

/// Adds (or replaces) an MCP server in the live manager state and persists it
/// to `config.json`'s `mcp_servers` array; connects immediately when the
/// server is enabled.
pub struct McpAddServerTool {
    manager: Arc<McpManager>,
    /// Guarded so two concurrent calls (parallel tool execution) cannot
    /// interleave their config read-modify-writes.
    config_lock: Arc<Mutex<()>>,
}

impl McpAddServerTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self {
            manager,
            config_lock: Arc::new(Mutex::new(())),
        }
    }
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

impl Tool for McpAddServerTool {
    fn name(&self) -> &str {
        "mcp_add_server"
    }

    fn description(&self) -> &str {
        "Add (or replace) an MCP server in WuffAgent's config. Transport \
         'stdio' (default): command (required) + args, env, working_dir. \
         Transport 'http': url (required) + headers (object). enabled \
         (default true) auto-connects at startup and connects immediately \
         unless connect_now=false. timeout_secs default 60. allowed_tools: \
         allowlist of the server's tool names (empty = all). The change is \
         persisted to config.json's mcp_servers array and applied live."
    }

    fn parameters_schema(&self) -> ToolSchema {
        fn opt(desc: &str) -> FieldSchema {
            FieldSchema {
                type_name: "string".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        }
        fn opt_list(desc: &str) -> FieldSchema {
            FieldSchema {
                type_name: "array".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        }
        fn opt_obj(desc: &str) -> FieldSchema {
            FieldSchema {
                type_name: "object".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        }
        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Unique server name; letters, digits, '_' and '-' only \
                              (it becomes part of the mcp__<server>__<tool> tool names)"
                    .to_string(),
                nullable: false,
            },
        );
        props.insert(
            "transport".to_string(),
            opt("'stdio' (default) or 'http'"),
        );
        props.insert("command".to_string(), opt("stdio: program to spawn"));
        props.insert(
            "args".to_string(),
            opt_list("stdio: command line arguments"),
        );
        props.insert("env".to_string(), opt_obj("stdio: extra environment variables"));
        props.insert(
            "working_dir".to_string(),
            opt("stdio: working directory for the spawned process"),
        );
        props.insert("url".to_string(), opt("http: Streamable HTTP endpoint URL"));
        props.insert(
            "headers".to_string(),
            opt_obj("http: request headers (e.g. Authorization)"),
        );
        props.insert(
            "enabled".to_string(),
            FieldSchema {
                type_name: "boolean".to_string(),
                description: "Enabled (default true): auto-connect at startup + connect now"
                    .to_string(),
                nullable: true,
            },
        );
        props.insert(
            "connect_now".to_string(),
            FieldSchema {
                type_name: "boolean".to_string(),
                description: "Connect immediately after saving (default: the enabled value)"
                    .to_string(),
                nullable: true,
            },
        );
        props.insert(
            "timeout_secs".to_string(),
            FieldSchema {
                type_name: "number".to_string(),
                description: "Per-call timeout in seconds (default 60)".to_string(),
                nullable: true,
            },
        );
        props.insert(
            "allowed_tools".to_string(),
            opt_list("Allowlist of this server's tool names (empty = all)"),
        );
        ToolSchema {
            name: "mcp_add_server".to_string(),
            description: "Add or replace an MCP server (persisted to config.json)"
                .to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let cfg = parse_server_config(&params).map_err(ToolError::InvalidParams)?;
        let existing: Vec<String> = self
            .manager
            .snapshot()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let other_names: Vec<String> = existing
            .iter()
            .filter(|n| n.as_str() != cfg.name)
            .cloned()
            .collect();
        cfg.validate(&other_names).map_err(ToolError::InvalidParams)?;
        let is_new = !existing.contains(&cfg.name);
        let connect = params
            .get::<bool>("connect_now")
            .unwrap_or(cfg.enabled);

        self.manager
            .upsert_server(cfg.clone())
            .map_err(mcp_err)?;

        let _guard = self.config_lock.lock().unwrap_or_else(|e| e.into_inner());
        let (_updated, warn) = update_mcp_servers_in_config(|current| {
            let mut list: Vec<McpServerConfig> = current.to_vec();
            match list.iter_mut().find(|c| c.name == cfg.name) {
                Some(slot) => *slot = cfg.clone(),
                None => list.push(cfg.clone()),
            }
            list
        });

        let mut result = serde_json::json!({
            "status": if is_new { "added" } else { "updated" },
            "server": cfg.name,
            "transport": cfg.transport_summary(),
            "enabled": cfg.enabled,
            "persisted": warn.is_none(),
        });
        if let Some(w) = warn {
            result["warning"] = serde_json::json!(w);
        }

        if connect {
            let name = cfg.name.clone();
            let manager = self.manager.clone();
            match run_mcp_op("MCP connect", std::time::Duration::from_secs(30), move || {
                manager.connect_sync(&name)
            }) {
                Ok(count) => {
                    result["connected_tools"] = serde_json::json!(count);
                }
                Err(e) => {
                    result["connection_error"] = serde_json::json!(e);
                    result["note"] = serde_json::json!("The server is saved; retry with mcp_connect");
                }
            }
        } else {
            result["note"] = serde_json::json!("Saved but not connected; use mcp_connect");
        }
        Ok(ToolOutput::Success(result))
    }
}

// ─── mcp_connect / mcp_disconnect ─────────────────────────────────────────────

/// Connects a configured MCP server (handshake + tools/list + tool
/// registration).
pub struct McpConnectTool {
    manager: Arc<McpManager>,
}

impl McpConnectTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpConnectTool {
    fn name(&self) -> &str {
        "mcp_connect"
    }

    fn description(&self) -> &str {
        "Connect an MCP server: runs the handshake, lists its tools and \
         registers the enabled ones as `mcp__<server>__<tool>` tools. Use it \
         after mcp_add_server (with connect_now=false) or to recover a server \
         in an error state."
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Name of the configured MCP server".to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "mcp_connect".to_string(),
            description: "Connect an MCP server".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let name: String = params
            .get("name")
            .ok_or_else(|| ToolError::InvalidParams("name is required".to_string()))?;
        let name = name.trim().to_string();
        let manager = self.manager.clone();
        let name_for_op = name.clone();
        let count = run_mcp_op("MCP connect", std::time::Duration::from_secs(30), move || {
            manager.connect_sync(&name_for_op)
        })
        .map_err(ToolError::Execution)?;
        Ok(ToolOutput::Success(serde_json::json!({
            "status": "connected",
            "server": name,
            "connected_tools": count,
        })))
    }
}

/// Disconnects an MCP server (unregisters its tools, kills the child process
/// for stdio).
pub struct McpDisconnectTool {
    manager: Arc<McpManager>,
}

impl McpDisconnectTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpDisconnectTool {
    fn name(&self) -> &str {
        "mcp_disconnect"
    }

    fn description(&self) -> &str {
        "Disconnect an MCP server: unregisters all of its `mcp__<server>__*` \
         tools and closes the connection (kills the child process for the \
         stdio transport). The server stays configured."
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Name of the configured MCP server".to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "mcp_disconnect".to_string(),
            description: "Disconnect an MCP server".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let name: String = params
            .get("name")
            .ok_or_else(|| ToolError::InvalidParams("name is required".to_string()))?;
        let name = name.trim().to_string();
        let manager = self.manager.clone();
        let name_for_op = name.clone();
        run_mcp_op(
            "MCP disconnect",
            std::time::Duration::from_secs(10),
            move || manager.disconnect_sync(&name_for_op),
        )
        .map_err(ToolError::Execution)?;
        Ok(ToolOutput::Success(serde_json::json!({
            "status": "disconnected",
            "server": name,
        })))
    }
}

// ─── mcp_remove_server ────────────────────────────────────────────────────────

/// Removes an MCP server from the live state and from `config.json` (its
/// tools are unregistered, its process killed).
pub struct McpRemoveServerTool {
    manager: Arc<McpManager>,
    config_lock: Arc<Mutex<()>>,
}

impl McpRemoveServerTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self {
            manager,
            config_lock: Arc::new(Mutex::new(())),
        }
    }
}

impl Tool for McpRemoveServerTool {
    fn name(&self) -> &str {
        "mcp_remove_server"
    }

    fn description(&self) -> &str {
        "Remove an MCP server: disconnects it (killing its process for the \
         stdio transport) and deletes it from config.json's mcp_servers \
         array. Use mcp_disconnect to keep the configuration."
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Name of the configured MCP server".to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "mcp_remove_server".to_string(),
            description: "Remove an MCP server (live + config.json)".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let name: String = params
            .get("name")
            .ok_or_else(|| ToolError::InvalidParams("name is required".to_string()))?;
        let name = name.trim().to_string();
        if self.manager.get_config(&name).is_err() {
            return Err(ToolError::InvalidParams(format!(
                "No MCP server named '{}' is configured",
                name
            )));
        }

        let manager = self.manager.clone();
        let name_for_op = name.clone();
        run_mcp_op("MCP remove", std::time::Duration::from_secs(10), move || {
            manager.remove_server_sync(&name_for_op)
        })
        .map_err(ToolError::Execution)?;

        let _guard = self.config_lock.lock().unwrap_or_else(|e| e.into_inner());
        let name_for_cfg = name.clone();
        let (_updated, warn) = update_mcp_servers_in_config(move |current| {
            current.iter().filter(|c| c.name != name_for_cfg).cloned().collect()
        });

        let mut result = serde_json::json!({
            "status": "removed",
            "server": name,
            "persisted": warn.is_none(),
        });
        if let Some(w) = warn {
            result["warning"] = serde_json::json!(w);
        }
        Ok(ToolOutput::Success(result))
    }
}

// ─── mcp_refresh_tools ────────────────────────────────────────────────────────

/// Re-runs `tools/list` over a live connection (the server's tool set may
/// have changed since connect).
pub struct McpRefreshToolsTool {
    manager: Arc<McpManager>,
}

impl McpRefreshToolsTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpRefreshToolsTool {
    fn name(&self) -> &str {
        "mcp_refresh_tools"
    }

    fn description(&self) -> &str {
        "Re-list the tools of a CONNECTED MCP server (its tool set may have \
         changed) and re-register the enabled ones. The server must be \
         connected first (mcp_connect)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Name of the connected MCP server".to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "mcp_refresh_tools".to_string(),
            description: "Re-list a connected MCP server's tools".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let name: String = params
            .get("name")
            .ok_or_else(|| ToolError::InvalidParams("name is required".to_string()))?;
        let name = name.trim().to_string();
        let manager = self.manager.clone();
        let name_for_op = name.clone();
        let count = run_mcp_op(
            "MCP refresh",
            std::time::Duration::from_secs(30),
            move || manager.refresh_tools_sync(&name_for_op),
        )
        .map_err(ToolError::Execution)?;
        Ok(ToolOutput::Success(serde_json::json!({
            "status": "refreshed",
            "server": name,
            "registered_tools": count,
        })))
    }
}

// ─── mcp_set_tool_enabled ─────────────────────────────────────────────────────

/// Enables or disables one tool of a connected server (registers or
/// unregisters its `mcp__<server>__<tool>` entry in the shared registry).
pub struct McpSetToolEnabledTool {
    manager: Arc<McpManager>,
}

impl McpSetToolEnabledTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

impl Tool for McpSetToolEnabledTool {
    fn name(&self) -> &str {
        "mcp_set_tool_enabled"
    }

    fn description(&self) -> &str {
        "Enable or disable ONE tool of a connected MCP server: enabling \
         registers it in the shared registry as `mcp__<server>__<tool>`, \
         disabling unregisters it. The flag is remembered across reconnects."
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = HashMap::new();
        props.insert(
            "server".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Name of the connected MCP server".to_string(),
                nullable: false,
            },
        );
        props.insert(
            "tool".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "The tool name as reported by the server".to_string(),
                nullable: false,
            },
        );
        props.insert(
            "enabled".to_string(),
            FieldSchema {
                type_name: "boolean".to_string(),
                description: "true to enable (register), false to disable (unregister)"
                    .to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "mcp_set_tool_enabled".to_string(),
            description: "Enable or disable one MCP server tool".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["server".to_string(), "tool".to_string(), "enabled".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let server: String = params
            .get("server")
            .ok_or_else(|| ToolError::InvalidParams("server is required".to_string()))?;
        let tool: String = params
            .get("tool")
            .ok_or_else(|| ToolError::InvalidParams("tool is required".to_string()))?;
        let enabled: bool = params
            .get("enabled")
            .ok_or_else(|| ToolError::InvalidParams("enabled is required".to_string()))?;
        self.manager
            .set_tool_enabled(&server, &tool, enabled)
            .map_err(mcp_err)?;
        let registry_name = crate::tools::mcp::mcp_tool_name(&server, &tool);
        Ok(ToolOutput::Success(serde_json::json!({
            "status": if enabled { "enabled" } else { "disabled" },
            "server": server,
            "tool": tool,
            "registry_name": registry_name,
        })))
    }
}

#[cfg(test)]
mod tests;
