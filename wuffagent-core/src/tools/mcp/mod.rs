//! MCP (Model Context Protocol) client + tool bridging.
//!
//! WuffAgent can connect to external MCP servers (stdio child process or
//! Streamable HTTP), discover their tools via `tools/list` and expose them
//! in the shared `ToolRegistry` as `mcp__<server>__<tool>` tools.
//!
//! Concurrency: all MCP I/O runs on a **dedicated 2-worker tokio runtime**
//! owned by [`McpManager`] (the UI thread lives inside the main runtime,
//! where `block_on` is not allowed). See `manager.rs` for details.
//!
//! Protocol: line-delimited JSON-RPC 2.0 (MCP revision 2025-06-18),
//! hand-rolled in [`jsonrpc`] — no external SDK.

pub mod client;
pub mod jsonrpc;
pub mod manager;
pub mod tool;
pub mod transport_http;
pub mod transport_stdio;

use thiserror::Error;

/// Errors surfaced by MCP operations (transports, handshake, tool calls).
#[derive(Debug, Error)]
pub enum McpError {
    #[error("Failed to spawn MCP server: {0}")]
    Spawn(String),
    #[error("MCP handshake failed: {0}")]
    Handshake(String),
    #[error("MCP request timed out: {0}")]
    Timeout(String),
    #[error("MCP JSON-RPC error {code}: {message}")]
    JsonRpc { code: i64, message: String },
    #[error("MCP server closed the connection: {0}")]
    ServerClosed(String),
    #[error("MCP server not connected: {0}")]
    NotConnected(String),
    #[error("MCP I/O error: {0}")]
    Io(String),
    #[error("MCP JSON error: {0}")]
    Json(String),
    #[error("MCP HTTP error: {0}")]
    Http(String),
    #[error("{0}")]
    Other(String),
}

pub use client::McpClient;
pub use jsonrpc::{
    parse_call_result, parse_tools_list, CallToolResult, JsonRpcError, JsonRpcRequest,
    JsonRpcResponse, McpContent, McpToolInfo, PROTOCOL_VERSION,
};
pub use manager::{
    McpManager, McpServerSnapshot, McpServerState, McpServerStatus, McpToolSnapshot,
};
pub use tool::{mcp_tool_name, sanitize_name_part, value_to_json_schema, McpTool, MCP_TOOL_PREFIX};
pub use transport_http::HttpTransport;
pub use transport_stdio::StdioTransport;

#[cfg(test)]
mod integration_tests {
    //! End-to-end test against a small mock MCP server speaking
    //! line-delimited JSON-RPC on stdio (spawned as a python process; the
    //! test skips itself if no python interpreter is available).

    use super::*;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::types::{ToolOutput, TracingToolLogger};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A minimal MCP server: `echo` (returns the input) and `fail_tool`
    /// (returns isError=true).
    const MOCK_SERVER_PY: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except Exception:
        continue
    m = req.get("method")
    rid = req.get("id")
    if m == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": req.get("params", {}).get("protocolVersion", "2025-06-18"),
            "capabilities": {},
            "serverInfo": {"name": "mock-mcp", "version": "0.0.0"}}})
    elif m == "notifications/initialized":
        pass
    elif m == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "echo", "description": "Echo back the input",
             "inputSchema": {"type": "object",
                              "properties": {"msg": {"type": "string"}},
                              "required": ["msg"]}},
            {"name": "fail_tool", "description": "Always fails",
             "inputSchema": {"type": "object", "properties": {}}}]}})
    elif m == "tools/call":
        p = req.get("params", {})
        if p.get("name") == "echo":
            msg = p.get("arguments", {}).get("msg", "")
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "echo: " + str(msg)}],
                "isError": False}})
        elif p.get("name") == "fail_tool":
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "boom"}], "isError": True}})
        else:
            send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32602, "message": "unknown tool"}})
"#;

    /// Find a usable python interpreter, or None (test skips itself).
    fn find_python() -> Option<String> {
        let candidates: &[&str] = if cfg!(target_os = "windows") {
            &["python", "py", "python3"]
        } else {
            &["python3", "python"]
        };
        for candidate in candidates {
            let ok = std::process::Command::new(candidate)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Some(candidate.to_string());
            }
        }
        None
    }

    #[tokio::test]
    async fn stdio_handshake_list_and_call() {
        let Some(python) = find_python() else {
            eprintln!("python not available, skipping MCP stdio integration test");
            return;
        };
        let client = McpClient::connect_stdio(
            "mock",
            &python,
            &["-c".to_string(), MOCK_SERVER_PY.to_string()],
            &HashMap::new(),
            None,
            10,
        )
        .await
        .expect("handshake with mock server");
        assert_eq!(client.server_name_reported, "mock-mcp");

        let tools = client.list_tools().await.expect("tools/list");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert!(tools.iter().any(|t| t.name == "fail_tool"));

        let result = client
            .call_tool("echo", json!({"msg": "hello"}))
            .await
            .expect("tools/call echo");
        assert!(!result.is_error);
        assert_eq!(result.to_text(), "echo: hello");

        let failed = client.call_tool("fail_tool", json!({})).await.unwrap();
        assert!(failed.is_error);
        assert_eq!(failed.to_text(), "boom");

        // Unknown tool → JSON-RPC error from the server.
        let unknown = client.call_tool("nope", json!({})).await;
        assert!(matches!(
            unknown,
            Err(McpError::JsonRpc { code: -32602, .. })
        ));

        client.disconnect().await;
        // Disconnect is idempotent (kill + wait happens only once).
        client.disconnect().await;
    }

    #[tokio::test]
    async fn manager_connect_registers_and_unregisters_tools() {
        let Some(python) = find_python() else {
            eprintln!("python not available, skipping MCP manager integration test");
            return;
        };
        let registry = Arc::new(ToolRegistry::new(Vec::new(), Arc::new(TracingToolLogger)));
        let manager = McpManager::new(registry.clone());

        manager
            .upsert_server(crate::config::McpServerConfig {
                name: "mock".to_string(),
                transport: crate::config::McpTransport::Stdio {
                    command: python.clone(),
                    args: vec!["-c".to_string(), MOCK_SERVER_PY.to_string()],
                    env: HashMap::new(),
                    working_dir: None,
                },
                enabled: true,
                timeout_secs: 10,
                allowed_tools: Vec::new(),
            })
            .unwrap();

        // connect_sync blocks on the MCP runtime; this test runs inside a
        // runtime context, so call it from a plain helper thread and await
        // the result through a oneshot.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let m2 = manager.clone();
        std::thread::spawn(move || {
            let res = m2.connect_sync("mock");
            let _ = tx.send(res);
        });
        let count = rx.await.unwrap().expect("connect to mock server");
        assert_eq!(count, 2, "both mock tools registered");

        // Tools are in the shared registry with the right names.
        let defs = registry.to_tool_definitions();
        let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
        assert!(names.contains(&"mcp__mock__echo".to_string()));
        assert!(names.contains(&"mcp__mock__fail_tool".to_string()));

        // Execute through the McpTool adapter (sync Tool trait), on a helper
        // thread — mimicking ToolManager's spawn_blocking (blocking_recv is
        // not allowed on a runtime context thread? It is, but mirror prod).
        let tool = registry.get("mcp__mock__echo").unwrap();
        let mut values = HashMap::new();
        values.insert("msg".to_string(), json!("via-registry"));
        let params = crate::tools::types::ToolParams { values };
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(tool.execute(params));
        });
        match rx.await.unwrap().unwrap() {
            ToolOutput::Success(v) => assert_eq!(v, json!("echo: via-registry")),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        }

        // Disable one tool → unregistered; re-enable → registered.
        manager
            .set_tool_enabled("mock", "fail_tool", false)
            .unwrap();
        assert!(registry.get("mcp__mock__fail_tool").is_none());
        manager.set_tool_enabled("mock", "fail_tool", true).unwrap();
        assert!(registry.get("mcp__mock__fail_tool").is_some());

        // Disconnect → all tools gone.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let m3 = manager.clone();
        std::thread::spawn(move || {
            let res = m3.disconnect_sync("mock");
            let _ = tx.send(res);
        });
        rx.await.unwrap().unwrap();
        assert!(registry.get("mcp__mock__echo").is_none());

        // Remove + shutdown (runtime dropped on a plain thread).
        let (tx, rx) = tokio::sync::oneshot::channel();
        let m4 = manager.clone();
        std::thread::spawn(move || {
            let res = m4.remove_server_sync("mock");
            let _ = tx.send(res);
        });
        rx.await.unwrap().unwrap();
        manager.shutdown();
    }
}
