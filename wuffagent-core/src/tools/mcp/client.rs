//! High-level MCP client: one live connection to one MCP server, regardless
//! of transport (stdio child process or Streamable HTTP).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::jsonrpc::{
    parse_call_result, parse_tools_list, CallToolResult, McpToolInfo, PROTOCOL_VERSION,
};
use super::transport_http::HttpTransport;
use super::transport_stdio::StdioTransport;
use super::McpError;

/// The transport behind one client connection.
pub enum Transport {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

impl Transport {
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        match self {
            Transport::Stdio(t) => t.request(method, params, timeout).await,
            Transport::Http(t) => t.request(method, params, timeout).await,
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        match self {
            Transport::Stdio(t) => t.notify(method, params).await,
            Transport::Http(t) => t.notify(method, params).await,
        }
    }

    /// Tear down the connection (kills the child process for stdio).
    pub async fn disconnect(&self) {
        match self {
            Transport::Stdio(t) => t.kill().await,
            Transport::Http(t) => t.kill().await,
        }
    }
}

/// A connected MCP client. Cheap to clone (the transport is shared); the
/// manager stores `Arc<McpClient>` so in-flight tool calls keep working while
/// the connection state is swapped.
#[derive(Clone)]
pub struct McpClient {
    transport: Arc<Transport>,
    /// Per-request timeout (from the server config).
    timeout: Duration,
    /// `serverInfo.name` from the `initialize` response (for display).
    pub server_name_reported: String,
}

impl McpClient {
    /// Connect to a stdio MCP server: spawn the process and run the
    /// `initialize` handshake.
    pub async fn connect_stdio(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&str>,
        timeout_secs: u64,
    ) -> Result<Self, McpError> {
        let transport = StdioTransport::spawn(server_name, command, args, env, working_dir).await?;
        Self::handshake(Arc::new(Transport::Stdio(transport)), timeout_secs).await
    }

    /// Connect to an HTTP (Streamable HTTP) MCP server.
    pub async fn connect_http(
        server_name: &str,
        url: &str,
        headers: &HashMap<String, String>,
        timeout_secs: u64,
    ) -> Result<Self, McpError> {
        let transport = HttpTransport::new(server_name, url, headers).await?;
        Self::handshake(Arc::new(Transport::Http(transport)), timeout_secs).await
    }

    /// The `initialize` request + `notifications/initialized` follow-up.
    async fn handshake(transport: Arc<Transport>, timeout_secs: u64) -> Result<Self, McpError> {
        let timeout = Duration::from_secs(timeout_secs.max(5));
        let init_params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "wuffagent",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let result = transport
            .request("initialize", init_params, timeout)
            .await
            .map_err(|e| McpError::Handshake(e.to_string()))?;
        let server_name_reported = result
            .get("serverInfo")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_string();

        // The client MUST signal readiness after the initialize response.
        let _ = transport
            .notify("notifications/initialized", json!({}))
            .await;

        tracing::info!(
            target: "mcp",
            server = server_name_reported,
            "MCP handshake complete"
        );

        Ok(Self {
            transport,
            timeout,
            server_name_reported,
        })
    }

    /// List the server's tools (follows `cursor` pagination, capped).
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools: Vec<McpToolInfo> = Vec::new();
        let mut cursor: Option<String> = None;
        for _page in 0..10 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self
                .transport
                .request("tools/list", params, self.timeout)
                .await?;
            tools.extend(parse_tools_list(&result));
            match result.get("cursor").and_then(|c| c.as_str()) {
                Some(next) => cursor = Some(next.to_string()),
                None => break,
            }
        }
        Ok(tools)
    }

    /// Call one of the server's tools.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .transport
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                self.timeout,
            )
            .await?;
        Ok(parse_call_result(&result))
    }

    /// Disconnect (kills the child process for stdio transports).
    pub async fn disconnect(&self) {
        self.transport.disconnect().await;
    }
}
