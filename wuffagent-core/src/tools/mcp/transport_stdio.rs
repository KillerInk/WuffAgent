//! Line-delimited JSON-RPC over the MCP server's stdio.
//!
//! Spawns the server as a child process, writes newline-delimited
//! JSON-RPC requests to its stdin and routes responses back to waiters by
//! request id. A reader task consumes stdout; a drain task logs stderr.
//! Disconnecting kills the child (kill + wait, to avoid zombies on Windows).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command, ChildStdin};
use tokio::sync::oneshot;

use super::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use super::McpError;

/// A live stdio JSON-RPC connection to one MCP server process.
pub struct StdioTransport {
    server_name: String,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    /// `None` once killed (so disconnect is idempotent).
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    /// In-flight request id → response receiver. A plain std `Mutex` is fine:
    /// guards are never held across an await point here.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: Arc<AtomicU64>,
}

impl StdioTransport {
    /// Spawn the server process and start the stdout/stderr tasks.
    pub async fn spawn(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&str>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        if let Some(wd) = working_dir {
            if !wd.is_empty() {
                cmd.current_dir(wd);
            }
        }
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| McpError::Spawn(format!("'{command}' failed to start: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn("failed to pipe server stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("failed to pipe server stdout".to_string()))?;
        let stderr = child.stderr.take();

        // Drain stderr so the pipe buffer never fills up and blocks the server;
        // each line goes to the `mcp` tracing target.
        if let Some(stderr) = stderr {
            let stderr_name = server_name.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "mcp", server = %stderr_name, stderr = %line, "MCP server stderr");
                }
            });
        }

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let name_for_reader = server_name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    tracing::debug!(target: "mcp", server = %name_for_reader, "Ignored non-JSON line from MCP server: {line}");
                    continue;
                };
                // Responses carry a numeric id plus result/error; everything
                // else (server-initiated notifications) is ignored in v1.
                let Some(id) = value.get("id").and_then(|i| i.as_u64()) else {
                    continue;
                };
                let has_payload = value.get("result").is_some() || value.get("error").is_some();
                if !has_payload {
                    continue;
                }
                if let Some(response) =
                    serde_json::from_value::<JsonRpcResponse>(value.clone()).ok()
                {
                    if let Some(tx) = pending_reader.lock().unwrap().remove(&id) {
                        let _ = tx.send(response);
                    }
                }
            }
            // stdout closed: drop any outstanding waiters (their `recv` will
            // return RecvError → ServerClosed).
            pending_reader.lock().unwrap().clear();
        });

        tracing::info!(target: "mcp", server = server_name, command, ?args, "MCP server spawned");

        Ok(Self {
            server_name: server_name.to_string(),
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            child: Arc::new(tokio::sync::Mutex::new(Some(child))),
            pending,
            next_id: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Send a request and wait (up to `timeout`) for its response.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let request = JsonRpcRequest::new(id, method, params);
        if let Err(e) = self.write_line(&request).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                if let Some(err) = response.error {
                    return Err(McpError::JsonRpc {
                        code: err.code,
                        message: format!("{} (method '{}')", err.message, method),
                    });
                }
                Ok(response.result.unwrap_or(Value::Null))
            }
            // The reader dropped the sender: the server process exited.
            Ok(Err(_)) => Err(McpError::ServerClosed(format!(
                "MCP server '{}' closed the connection during '{}'",
                self.server_name, method
            ))),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(McpError::Timeout(format!(
                    "'{}' on server '{}' timed out after {}s",
                    method,
                    self.server_name,
                    timeout.as_secs()
                )))
            }
        }
    }

    /// Send a notification (no response expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.write_line(&JsonRpcRequest::notification(method, params)).await
    }

    /// Kill the child process and wait for it to be reaped (idempotent).
    pub async fn kill(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            if let Err(e) = child.kill().await {
                tracing::debug!(target: "mcp", server = %self.server_name, error = %e, "kill failed (process likely already exited)");
            }
            let _ = child.wait().await;
            tracing::info!(target: "mcp", server = %self.server_name, "MCP server process terminated");
        }
    }

    async fn write_line(&self, request: &JsonRpcRequest) -> Result<(), McpError> {
        let mut line = serde_json::to_string(request).map_err(|e| McpError::Json(e.to_string()))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Io(format!("write to MCP server stdin: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Io(format!("flush MCP server stdin: {e}")))?;
        Ok(())
    }
}
