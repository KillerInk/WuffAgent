use serde::{Deserialize, Serialize};

/// A chat message with a role (system/user/assistant/tool) and content.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Tool call requests from the AI (non-null when the AI wants to invoke a tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// A single tool call requested by the AI.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolFunction,
}

/// The function specification within a tool call.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

/// Token usage statistics returned by the API.
#[derive(Deserialize, Debug, Clone)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
