//! Minimal JSON-RPC 2.0 + MCP protocol types.
//!
//! WuffAgent speaks the Model Context Protocol (MCP) with a hand-rolled,
//! line-delimited JSON-RPC client (no external SDK). The types here cover
//! exactly the subset of MCP used by the app: `initialize`,
//! `notifications/initialized`, `tools/list` and `tools/call`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP protocol version we advertise during the `initialize` handshake.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC 2.0 request (a notification has `id: None`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: method.to_string(),
            params: Some(params),
        }
    }

    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.to_string(),
            params: Some(params),
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Try to interpret an arbitrary inbound line as a response to a known
    /// request id. Returns `None` for notifications, parse failures, or
    /// responses to other ids.
    pub fn from_value(value: &Value, expected_id: u64) -> Option<Self> {
        if value.get("id")?.as_u64()? != expected_id {
            return None;
        }
        // A JSON-RPC response must carry `result` or `error`.
        if value.get("result").is_none() && value.get("error").is_none() {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }
}

// ─── MCP protocol shapes ────────────────────────────────────────────────────

/// A tool advertised by an MCP server via `tools/list`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Raw JSON schema (kept verbatim; converted to the core `JsonSchema`
    /// in the tool adapter).
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

/// One content item of a `tools/call` result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        /// Base64-encoded image bytes.
        data: String,
        #[serde(default, rename = "mimeType")]
        mime_type: String,
    },
    /// Inline resource (text payload or a link, depending on server).
    Resource {
        resource: Value,
    },
}

impl McpContent {
    /// Best-effort text rendering for the model's context.
    pub fn to_text(&self) -> String {
        match self {
            McpContent::Text { text } => text.clone(),
            McpContent::Image { data, mime_type } => {
                format!("[image {mime_type}, {} base64 chars, omitted from text]", data.len())
            }
            McpContent::Resource { resource } => {
                let uri = resource
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .unwrap_or("<unknown uri>")
                    .to_string();
                match resource.get("text").and_then(|t| t.as_str()) {
                    Some(text) => format!("[resource {uri}]\n{text}"),
                    None => format!("[resource {uri}]"),
                }
            }
        }
    }
}

/// Decoded result of a `tools/call` request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    /// The server-side tool signalled a tool-level error (the JSON-RPC call
    /// itself succeeded).
    #[serde(default)]
    pub is_error: bool,
}

impl CallToolResult {
    /// Concatenated text rendering of all content parts.
    pub fn to_text(&self) -> String {
        self.content.iter().map(McpContent::to_text).collect::<Vec<_>>().join("\n")
    }
}

/// Parse the `tools/call` result object into a `CallToolResult`.
pub fn parse_call_result(result: &Value) -> CallToolResult {
    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    CallToolResult {
        content,
        is_error: result
            .get("isError")
            .and_then(|e| e.as_bool())
            .unwrap_or(false),
    }
}

/// Parse the `tools/list` result object into a list of tool infos.
pub fn parse_tools_list(result: &Value) -> Vec<McpToolInfo> {
    result
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| -> Option<McpToolInfo> {
                    serde_json::from_value(item.clone()).ok()
                })
                .filter(|t| !t.name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde_roundtrip() {
        let req = JsonRpcRequest::new(7, "tools/list", serde_json::json!({}));
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "tools/list");
        let back: JsonRpcRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn notification_has_no_id() {
        let req = JsonRpcRequest::notification("notifications/initialized", serde_json::json!({}));
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("id").is_none());
    }

    #[test]
    fn response_from_value_matches_id_and_shape() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {"ok": true}
        });
        assert!(JsonRpcResponse::from_value(&line, 3).is_some());
        assert!(JsonRpcResponse::from_value(&line, 4).is_none());
        // A notification (no result/error) is not a response.
        let note = serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "x"});
        assert!(JsonRpcResponse::from_value(&note, 3).is_none());
    }

    #[test]
    fn response_error_variant() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": {"code": -32601, "message": "Method not found"}
        });
        let resp = JsonRpcResponse::from_value(&line, 9).unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn parse_tools_list_roundtrip() {
        let result = serde_json::json!({
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo input",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"msg": {"type": "string"}},
                        "required": ["msg"]
                    }
                },
                {"name": "no_schema"}
            ]
        });
        let tools = parse_tools_list(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo input");
        assert_eq!(tools[0].input_schema["type"], "object");
        assert_eq!(tools[1].input_schema, serde_json::Value::Null);
    }

    #[test]
    fn parse_call_result_content() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "aW1n", "mimeType": "image/png"}
            ],
            "isError": true
        });
        let parsed = parse_call_result(&result);
        assert!(parsed.is_error);
        assert_eq!(parsed.content.len(), 2);
        assert_eq!(parsed.to_text().lines().next().unwrap(), "hello");
        assert!(parsed.to_text().contains("[image image/png"));
    }
}
