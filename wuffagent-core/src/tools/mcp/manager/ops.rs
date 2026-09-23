//! Async core of `McpManager` (run on the dedicated MCP runtime):
//! connect / disconnect / tool call / tool refresh. Split out of
//! `manager/mod.rs` (Phase D size split).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::McpTransport;

use crate::tools::mcp::client::McpClient;
use crate::tools::mcp::jsonrpc::CallToolResult;
use crate::tools::mcp::McpError;

use super::{McpManager, McpServerStatus};

impl McpManager {
    /// Connect: handshake → `tools/list` → register enabled tools.
    /// Replaces any previous connection for the server.
    pub async fn connect_async(&self, name: &str) -> Result<usize, McpError> {
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(McpError::Other("MCP manager is shutting down".to_string()));
        }
        let config = self.get_config(name)?;
        {
            let mut servers = self.inner.servers.write().unwrap();
            if let Some(state) = servers.get_mut(name) {
                state.status = McpServerStatus::Connecting;
            }
        }

        let client = match &config.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                working_dir,
            } => {
                McpClient::connect_stdio(
                    name,
                    command,
                    args,
                    env,
                    working_dir.as_deref(),
                    config.timeout_secs,
                )
                .await
            }
            McpTransport::Http { url, headers } => {
                McpClient::connect_http(name, url, headers, config.timeout_secs).await
            }
        };
        let client = match client {
            Ok(c) => Arc::new(c),
            Err(e) => {
                self.set_status(name, McpServerStatus::Error(e.to_string()));
                return Err(e);
            }
        };

        let tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                client.disconnect().await;
                self.set_status(name, McpServerStatus::Error(e.to_string()));
                return Err(e);
            }
        };
        let tools = if config.allowed_tools.is_empty() {
            tools
        } else {
            tools
                .into_iter()
                .filter(|t| config.allowed_tools.contains(&t.name))
                .collect()
        };

        // Swap in the new client (kill the previous one), keeping any
        // per-tool enable flags the user set earlier.
        let (old_client, previous_flags) = {
            let mut servers = self.inner.servers.write().unwrap();
            let state = servers.get_mut(name).ok_or_else(|| {
                McpError::Other(format!("server '{name}' removed during connect"))
            })?;
            let old = state.client.take();
            let flags = std::mem::take(&mut state.tool_enabled);
            (old, flags)
        };
        if let Some(old) = old_client {
            old.disconnect().await;
        }

        let mut registered = 0usize;
        let mut new_flags: HashMap<String, bool> = HashMap::new();
        for info in &tools {
            let enabled = previous_flags.get(&info.name).copied().unwrap_or(true);
            new_flags.insert(info.name.clone(), enabled);
            if enabled {
                if self.register_tool(name, info).is_some() {
                    registered += 1;
                }
            }
        }

        {
            let mut servers = self.inner.servers.write().unwrap();
            if let Some(state) = servers.get_mut(name) {
                state.tools = tools;
                state.tool_enabled = new_flags;
                state.client = Some(client);
                state.status = McpServerStatus::Connected {
                    tool_count: registered,
                };
            }
        }
        tracing::info!(target: "mcp", server = name, tools = registered, "MCP server connected");
        Ok(registered)
    }

    /// Disconnect: unregister all of the server's tools and kill the
    /// connection (child process for stdio).
    pub async fn disconnect_async(&self, name: &str) -> Result<(), McpError> {
        let known = {
            let servers = self.inner.servers.read().unwrap();
            servers.contains_key(name)
        };
        if !known {
            return Err(McpError::Other(format!("unknown MCP server '{name}'")));
        }
        // Unregister the tools first so agents can't call a dead server.
        let tools: Vec<String> = {
            let servers = self.inner.servers.read().unwrap();
            servers
                .get(name)
                .map(|s| s.tools.iter().map(|t| t.name.clone()).collect())
                .unwrap_or_default()
        };
        for tool in &tools {
            self.unregister_tool(name, tool);
        }
        let client = {
            let mut servers = self.inner.servers.write().unwrap();
            servers.get_mut(name).and_then(|s| s.client.take())
        };
        if let Some(client) = client {
            client.disconnect().await;
        }
        self.set_status(name, McpServerStatus::Configured);
        tracing::info!(target: "mcp", server = name, "MCP server disconnected");
        Ok(())
    }

    /// Call one of a server's tools (used by `McpTool::execute`).
    pub async fn call_tool_async(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let client = {
            let servers = self.inner.servers.read().unwrap();
            let state = servers
                .get(server)
                .ok_or_else(|| McpError::NotConnected(server.to_string()))?;
            state.client.clone()
        };
        let client = client.ok_or_else(|| McpError::NotConnected(server.to_string()))?;
        let result = client.call_tool(tool, arguments).await;
        match &result {
            Ok(r) if r.is_error => {
                tracing::warn!(target: "mcp", server, tool, "MCP tool call reported error")
            }
            Err(e) => {
                tracing::warn!(target: "mcp", server, tool, error = %e, "MCP tool call failed")
            }
            _ => {}
        }
        result
    }

    /// Re-list the server's tools and re-register them (replacing the old
    /// set). Keeps per-tool enable flags where the tool still exists.
    pub async fn refresh_tools_async(&self, name: &str) -> Result<usize, McpError> {
        let client = {
            let servers = self.inner.servers.read().unwrap();
            let state = servers
                .get(name)
                .ok_or_else(|| McpError::Other(format!("unknown MCP server '{name}'")))?;
            state
                .client
                .clone()
                .ok_or_else(|| McpError::NotConnected(name.to_string()))?
        };
        let config = self.get_config(name)?;
        let mut tools = client.list_tools().await?;
        if !config.allowed_tools.is_empty() {
            tools.retain(|t| config.allowed_tools.contains(&t.name));
        }

        // Replace the registered set: unregister everything, re-register.
        let mut servers = self.inner.servers.write().unwrap();
        let state = servers
            .get_mut(name)
            .ok_or_else(|| McpError::Other(format!("unknown MCP server '{name}'")))?;
        for old in &state.tools {
            self.unregister_tool(name, &old.name);
        }
        let mut registered = 0usize;
        let mut new_flags: HashMap<String, bool> = HashMap::new();
        for info in &tools {
            let enabled = state.tool_enabled.get(&info.name).copied().unwrap_or(true);
            new_flags.insert(info.name.clone(), enabled);
            if enabled {
                if self.register_tool(name, info).is_some() {
                    registered += 1;
                }
            }
        }
        state.tools = tools;
        state.tool_enabled = new_flags;
        state.status = McpServerStatus::Connected {
            tool_count: registered,
        };
        Ok(registered)
    }
}
