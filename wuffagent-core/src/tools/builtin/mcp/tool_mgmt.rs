//! Introspection + per-tool registration: `mcp_list` / `mcp_refresh_tools` /
//! `mcp_set_tool_enabled`. Shared helpers (`status_str`, `run_mcp_op`,
//! `mcp_err`) live in [`super`] and are pulled in with `use super::*;`.

use super::*;

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
