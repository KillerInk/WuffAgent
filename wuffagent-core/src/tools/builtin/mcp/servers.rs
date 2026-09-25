//! Server lifecycle tools: `mcp_add_server` / `mcp_connect` / `mcp_disconnect`
//! / `mcp_remove_server`. Shared helpers (config read-modify-write,
//! `run_mcp_op`, `parse_server_config`, event notification) live in
//! [`super`] and are pulled in with `use super::*;`.

use super::*;

// ─── mcp_add_server ───────────────────────────────────────────────────────────

/// Adds (or replaces) an MCP server in the live manager state and persists it
/// to `config.json`'s `mcp_servers` array; connects immediately when the
/// server is enabled.
pub struct McpAddServerTool {
    manager: Arc<McpManager>,
    /// Guarded so two concurrent calls (parallel tool execution) cannot
    /// interleave their config read-modify-writes.
    config_lock: Arc<Mutex<()>>,
    /// Core → UI event channel (optional; set by the registration helper so
    /// config changes can notify the UI). `session_id` identifies the agent
    /// run that triggered the change.
    events: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    session_id: Option<String>,
}

impl McpAddServerTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self {
            manager,
            config_lock: Arc::new(Mutex::new(())),
            events: None,
            session_id: None,
        }
    }

    /// Attach the core → UI event channel so config changes can notify the UI.
    pub fn with_events(
        mut self,
        events: Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>,
        session_id: Option<String>,
    ) -> Self {
        self.events = Some(events);
        self.session_id = session_id;
        self
    }
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
         persisted to config.json's mcp_servers array and applied live. \
         NOTE: the live state of THIS process is the single source of truth — \
         WuffAgent keeps config.json in memory and never reloads it, and the \
         alternate self-restart build (other target/) reads the file only at \
         ITS startup; so a server added at runtime may be missing in the \
         alternate build until you add it there too (mcp_add_server) or \
         restart the app normally (from outside WuffAgent)."
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
        // Tell the UI its in-memory mcp_servers list is stale (it must reload
        // config.json so the MCP panel matches what the tool just wrote).
        notify_config_changed(&self.events, self.session_id.as_deref());

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
    events: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    session_id: Option<String>,
}

impl McpRemoveServerTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self {
            manager,
            config_lock: Arc::new(Mutex::new(())),
            events: None,
            session_id: None,
        }
    }

    /// Attach the core → UI event channel so config changes can notify the UI.
    pub fn with_events(
        mut self,
        events: Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>,
        session_id: Option<String>,
    ) -> Self {
        self.events = Some(events);
        self.session_id = session_id;
        self
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
        notify_config_changed(&self.events, self.session_id.as_deref());

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
