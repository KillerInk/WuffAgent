//! MCP Streamable HTTP transport (best-effort v1).
//!
//! Each JSON-RPC request is a POST of the request body to the server URL with
//! `Accept: application/vnd.api+json, text/event-stream`. The response is
//! either a single JSON object or an SSE stream; we read `data:` lines until
//! we see the response matching our request id, then stop. The
//! `Mcp-Session-Id` header returned by `initialize` is round-tripped on all
//! subsequent requests. Legacy SSE-only servers (separate GET stream) are a
//! documented follow-up.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};

use super::jsonrpc::JsonRpcRequest;
use super::McpError;

const MCP_SESSION_HEADER: &str = "Mcp-Session-Id";

pub struct HttpTransport {
    server_name: String,
    client: reqwest::Client,
    url: String,
    extra_headers: HashMap<String, String>,
    /// Session id from the `initialize` response, if the server assigns one.
    session_id: tokio::sync::Mutex<Option<String>>,
    next_id: AtomicU64,
}

impl HttpTransport {
    pub async fn new(
        server_name: &str,
        url: &str,
        extra_headers: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let client = reqwest::Client::new();
        Ok(Self {
            server_name: server_name.to_string(),
            client,
            url: url.to_string(),
            extra_headers: extra_headers.clone(),
            session_id: tokio::sync::Mutex::new(None),
            next_id: AtomicU64::new(0),
        })
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        let request = JsonRpcRequest::new(id, method, params);
        let body = serde_json::to_string(&request).map_err(|e| McpError::Json(e.to_string()))?;

        let mut req = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("Accept", "application/vnd.api+json, text/event-stream")
            .body(body)
            .timeout(timeout);
        for (key, value) in &self.extra_headers {
            req = req.header(key, value);
        }
        let session = self.session_id.lock().await.clone();
        if let Some(sid) = session {
            req = req.header(MCP_SESSION_HEADER, sid);
        }

        let response = req
            .send()
            .await
            .map_err(|e| McpError::Http(format!("POST {} failed: {e}", self.url)))?;

        // Persist the session id assigned by the server (initialize response).
        if let Some(sid) = response.headers().get(MCP_SESSION_HEADER).and_then(|v| v.to_str().ok())
        {
            if !sid.is_empty() {
                *self.session_id.lock().await = Some(sid.to_string());
            }
        }

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(McpError::Http(format!(
                "server '{}' returned HTTP {}: {}",
                self.server_name,
                status,
                text.chars().take(300).collect::<String>()
            )));
        }

        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));

        if is_sse {
            // Read SSE `data:` lines until we find our response id.
            let mut stream = response.bytes_stream();
            let mut line: Vec<u8> = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| McpError::Http(format!("SSE stream: {e}")))?;
                for byte in chunk {
                    if byte == b'\n' {
                        let s = String::from_utf8_lossy(&line).to_string();
                        line.clear();
                        let s = s.trim_end_matches('\r');
                        if let Some(payload) = s.strip_prefix("data:") {
                            let payload = payload.trim();
                            if payload.is_empty() {
                                continue;
                            }
                            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                                continue;
                            };
                            if let Some(resp_id) = value.get("id").and_then(|i| i.as_u64()) {
                                if resp_id == id
                                    && (value.get("result").is_some()
                                        || value.get("error").is_some())
                                {
                                    return finish_jsonrpc(value);
                                }
                            }
                        }
                    } else {
                        line.push(byte);
                    }
                }
            }
            // Stream closed without our response (server may have sent the
            // response as plain JSON on another path, or hung up early).
            Err(McpError::Http(format!(
                "SSE stream from '{}' ended without a response for id {id}",
                self.server_name
            )))
        } else {
            // Plain JSON response body.
            let text = response
                .text()
                .await
                .map_err(|e| McpError::Http(format!("reading body: {e}")))?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| McpError::Http(format!("non-JSON response from '{}': {e}", self.server_name)))?;
            finish_jsonrpc(value)
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let request = JsonRpcRequest::notification(method, params);
        let body = serde_json::to_string(&request).map_err(|e| McpError::Json(e.to_string()))?;

        let mut req = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("Accept", "application/vnd.api+json, text/event-stream")
            .body(body);
        for (key, value) in &self.extra_headers {
            req = req.header(key, value);
        }
        let session = self.session_id.lock().await.clone();
        if let Some(sid) = session {
            req = req.header(MCP_SESSION_HEADER, sid);
        }

        let response = req
            .send()
            .await
            .map_err(|e| McpError::Http(format!("POST {} failed: {e}", self.url)))?;
        // Notifications normally get 202 Accepted (no body); tolerate 200 too.
        let status = response.status();
        if !(status.is_success() || status == reqwest::StatusCode::ACCEPTED) {
            return Err(McpError::Http(format!(
                "notification '{}' returned HTTP {status}",
                method
            )));
        }
        // Drain the (usually empty) body so the connection can be reused.
        let _ = response.text().await;
        Ok(())
    }

    /// No persistent resources to release for HTTP (connections are pooled).
    pub async fn kill(&self) {
        *self.session_id.lock().await = None;
    }
}

/// Convert a raw JSON-RPC response object into its `result` (or an error).
fn finish_jsonrpc(value: Value) -> Result<Value, McpError> {
    let response: super::jsonrpc::JsonRpcResponse = serde_json::from_value(value)
        .map_err(|e| McpError::Http(format!("malformed JSON-RPC response: {e}")))?;
    if let Some(err) = response.error {
        return Err(McpError::JsonRpc {
            code: err.code,
            message: err.message,
        });
    }
    Ok(response.result.unwrap_or_else(|| json!(null)))
}
