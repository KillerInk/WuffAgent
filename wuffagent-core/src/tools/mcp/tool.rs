//! `McpTool` — adapts one tool of one MCP server to the core (sync) `Tool`
//! trait, bridging to the async `McpManager` on its dedicated runtime.
//!
//! The core `Tool` trait is synchronous and (in the app) runs on
//! `ToolManager`'s `spawn_blocking` thread. That thread is inside a tokio
//! runtime context, so `block_on` is not allowed there — instead the call is
//! spawned on the MCP runtime and awaited with a `oneshot::blocking_recv`.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;

use super::jsonrpc::{CallToolResult, McpToolInfo};
use super::manager::McpManager;
use super::McpError;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

/// Prefix for all MCP tool names.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// Maximum bytes of MCP tool output fed back to the model (larger results
/// are truncated with a marker).
pub const MAX_MCP_OUTPUT_BYTES: usize = 100 * 1024;

/// Map characters outside `[a-zA-Z0-9_-]` to `_` (OpenAI function names must
/// match `^[a-zA-Z0-9_-]{1,64}$`).
pub fn sanitize_name_part(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Full registry name for an MCP tool: `mcp__<server>__<tool>`, truncated to
/// 64 chars (the sanitized name is pure ASCII, so char truncation is safe).
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    let name = format!(
        "mcp__{}__{}",
        sanitize_name_part(server),
        sanitize_name_part(tool)
    );
    name.chars().take(64).collect()
}

/// Convert a raw MCP `inputSchema` JSON value into the core `JsonSchema`.
///
/// Simple schemas map cleanly; exotic ones (nested objects, `anyOf` beyond
/// nullable, `$ref`) degrade gracefully to a flat `object` with the top-level
/// string properties only.
pub fn value_to_json_schema(value: &Value) -> JsonSchema {
    let type_name = value
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("object")
        .to_string();

    let mut properties: std::collections::HashMap<String, FieldSchema> =
        std::collections::HashMap::new();
    if let Some(props) = value.get("properties").and_then(|p| p.as_object()) {
        for (key, prop) in props {
            // `anyOf: [..., {"type": "null"}]` is the common JSON-Schema nullable marker.
            let nullable = prop
                .get("nullable")
                .and_then(|n| n.as_bool())
                .unwrap_or(false)
                || matches!(
                    prop.get("anyOf"),
                    Some(Value::Array(items))
                        if items
                            .iter()
                            .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("null"))
                );
            properties.insert(
                key.clone(),
                FieldSchema {
                    type_name: prop
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("string")
                        .to_string(),
                    description: prop
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    nullable,
                },
            );
        }
    }

    let required = value
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    JsonSchema {
        type_name,
        properties: if properties.is_empty() {
            None
        } else {
            Some(properties)
        },
        required,
    }
}

/// Core `Tool` implementation for one tool exposed by an MCP server.
pub struct McpTool {
    /// Registry name (`mcp__<server>__<tool>`).
    full_name: String,
    server_name: String,
    tool_name: String,
    description: String,
    input_schema: JsonSchema,
    manager: McpManager,
}

impl McpTool {
    pub fn new(manager: McpManager, server_name: &str, info: &McpToolInfo) -> Self {
        let full_name = mcp_tool_name(server_name, &info.name);
        let description = format!("[MCP: {server_name}] {}", info.description);
        Self {
            full_name,
            server_name: server_name.to_string(),
            tool_name: info.name.clone(),
            description,
            input_schema: value_to_json_schema(&info.input_schema),
            manager,
        }
    }

    /// Raw tool name as reported by the server.
    pub fn raw_name(&self) -> &str {
        &self.tool_name
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.full_name.clone(),
            description: self.description.clone(),
            input_type: Some(self.input_schema.clone()),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let arguments = serde_json::to_value(&params.values)
            .map_err(|e| ToolError::Execution(format!("serializing MCP arguments: {e}")))?;

        let manager = self.manager.clone();
        let server = self.server_name.clone();
        let tool = self.tool_name.clone();
        let (tx, rx) = oneshot::channel();

        let spawned = manager.runtime().map(|rt| {
            rt.spawn(async move {
                let timeout_secs = manager.timeout_secs(&server);
                let result = tokio::time::timeout(
                    Duration::from_secs(timeout_secs),
                    manager.call_tool_async(&server, &tool, arguments),
                )
                .await;
                let _ = tx.send(result);
            })
        });
        let Some(handle) = spawned else {
            return Err(ToolError::Execution(
                "MCP runtime is not available (manager shutting down?)".to_string(),
            ));
        };

        let outcome = match rx.blocking_recv() {
            Ok(inner) => inner,
            Err(_) => {
                handle.abort();
                return Err(ToolError::Execution(
                    "MCP manager went away during the tool call".to_string(),
                ));
            }
        };

        match outcome {
            Ok(Ok(result)) => Ok(call_result_to_output(result)),
            Ok(Err(McpError::Timeout(message))) => Ok(ToolOutput::Error(message)),
            Ok(Err(e)) => Err(ToolError::Execution(format!(
                "MCP tool '{}': {e}",
                self.tool_name
            ))),
            Err(_) => Ok(ToolOutput::Error(format!(
                "MCP tool '{}' timed out (server unresponsive)",
                self.tool_name
            ))),
        }
    }
}

/// Map a `tools/call` result to a `ToolOutput` (truncating large payloads).
fn call_result_to_output(result: CallToolResult) -> ToolOutput {
    let text = truncate_to_bytes(&result.to_text(), MAX_MCP_OUTPUT_BYTES);
    if result.is_error {
        return ToolOutput::Error(text);
    }
    // Structured output when the server returned JSON, plain string otherwise.
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => ToolOutput::Success(v),
        Err(_) => ToolOutput::Success(Value::String(text)),
    }
}

fn truncate_to_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_and_name_format() {
        assert_eq!(sanitize_name_part("my server!"), "my_server_");
        assert_eq!(mcp_tool_name("fs", "read"), "mcp__fs__read");
        assert_eq!(
            mcp_tool_name("My Server", "do:thing"),
            "mcp__My_Server__do_thing"
        );
        let long = mcp_tool_name("s", &"x".repeat(100));
        assert_eq!(long.len(), 64);
        // Always matches ^[a-zA-Z0-9_-]{1,64}$
        let re_ok = |n: &str| {
            n.len() <= 64
                && !n.is_empty()
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        assert!(re_ok(&mcp_tool_name("a", "b")));
        assert!(re_ok(&long));
    }

    #[test]
    fn schema_conversion_simple() {
        let schema = value_to_json_schema(&json!({
            "type": "object",
            "properties": {
                "msg": {"type": "string", "description": "message"},
                "n": {"type": "integer"},
                "opt": {"anyOf": [{"type": "string"}, {"type": "null"}]}
            },
            "required": ["msg"]
        }));
        assert_eq!(schema.type_name, "object");
        let props = schema.properties.as_ref().unwrap();
        assert_eq!(props["msg"].type_name, "string");
        assert_eq!(props["msg"].description, "message");
        assert_eq!(props["n"].type_name, "integer");
        assert!(props["opt"].nullable, "anyOf-null means nullable");
        assert_eq!(schema.required, vec!["msg".to_string()]);
    }

    #[test]
    fn schema_conversion_fallback() {
        let schema = value_to_json_schema(&json!(null));
        assert_eq!(schema.type_name, "object");
        assert!(schema.properties.is_none());
        assert!(schema.required.is_empty());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "éééééé"; // 2 bytes per char
        let t = truncate_to_bytes(&s, 7);
        // 7 bytes is not a char boundary → back off to 6 bytes = 3 chars.
        assert!(t.starts_with("ééé"));
        assert!(t.ends_with("…[truncated]"));
        assert!(!t.chars().skip(3).next().map_or(false, |c| c == 'é'));
        let short = truncate_to_bytes("abc", 10);
        assert_eq!(short, "abc");
    }
}
