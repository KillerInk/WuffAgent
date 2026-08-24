use serde::{Deserialize, Serialize};

/// A chat message with a role (system/user/assistant/tool) and content.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
    /// Tool call requests from the AI (non-null when the AI wants to invoke a tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Reference to the tool call this result belongs to (for tool role messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Model reasoning/thinking content (llama.cpp `reasoning_content`, DeepSeek/Qwen style).
    /// Round-tripped so the model can see its own prior reasoning across tool-call rounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// A single tool call requested by the AI.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolFunction,
}

/// Reasoning effort level sent to the model server (Qwen3/llama.cpp style).
/// Serialized as a lowercase string; `Off` is omitted from requests entirely.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// JSON string value for the request body, or `None` for Off.
    pub fn as_wire_value(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Off => None,
            ReasoningEffort::Low => Some("low"),
            ReasoningEffort::Medium => Some("medium"),
            ReasoningEffort::High => Some("high"),
        }
    }

    /// Short name for dropdown items.
    pub fn name(self) -> &'static str {
        match self {
            ReasoningEffort::Off => "Off",
            ReasoningEffort::Low => "Low",
            ReasoningEffort::Medium => "Medium",
            ReasoningEffort::High => "High",
        }
    }

    /// Human-readable label for UI display.
    pub fn label(self) -> &'static str {
        match self {
            ReasoningEffort::Off => "Reasoning: Off",
            ReasoningEffort::Low => "Reasoning: Low",
            ReasoningEffort::Medium => "Reasoning: Medium",
            ReasoningEffort::High => "Reasoning: High",
        }
    }

    /// All selectable variants, in UI order.
    pub const VARIANTS: [ReasoningEffort; 4] = [
        ReasoningEffort::Off,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ];
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

/// UI application status indicator.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AppStatus {
    #[default]
    Stopped,
    Connecting,
    Ready,
    Generating,
    Error(String),
}

/// A chat message displayed in the UI.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// Events that flow from the client engine to the UI.
#[derive(Clone, Debug)]
pub enum AppEvent {
    StreamChunk { content: String },
    StreamComplete { content: String, usage: Option<Usage> },
    StreamError { error: String },
    ToolCallWarning { tool_name: String, message: String },
    ToolCallStart { tool_name: String, call_id: String },
    ToolCallComplete { tool_name: String, call_id: String, result: String },
    ToolCallError { tool_name: String, call_id: String, error: String },
    // Thinking output events (e.g. Claude-style reasoning)
    StreamThinkingChunk { content: String },
    StreamThinkingComplete { content: String },
    // Agent engine events
    AgentEngineComplete { response: String },
    AgentEngineError { error: String },
    AgentEngineStopped,
    // Agent chain events
    AgentChainStarted { agent_name: String, depth: u32 },
    AgentChainCompleted { agent_name: String, result: String, depth: u32 },
    AgentChainError { agent_name: String, error: String, depth: u32 },
    AgentChainCancelled { agent_name: String },
    AgentChainComplete { response: String, entries: Vec<crate::sessions::model::AgentChainEntry> },
    AgentChainStopped,
    /// Remote server n_ctx was updated.
    NCtxUpdated { n_ctx: u32 },
}

/// Convert engine events (from the chat engine) to app events (UI-facing).
impl From<crate::client::engine::EngineEvent> for AppEvent {
    fn from(event: crate::client::engine::EngineEvent) -> Self {
        match event {
            crate::client::engine::EngineEvent::StreamChunk { content } => Self::StreamChunk { content },
            crate::client::engine::EngineEvent::StreamComplete { content, usage } => Self::StreamComplete { content, usage },
            crate::client::engine::EngineEvent::StreamError { error } => Self::StreamError { error },
            crate::client::engine::EngineEvent::ToolCallStart { tool_name, call_id } => Self::ToolCallStart { tool_name, call_id },
            crate::client::engine::EngineEvent::ToolCallComplete { tool_name, call_id, result } => Self::ToolCallComplete { tool_name, call_id, result },
            crate::client::engine::EngineEvent::ToolCallError { tool_name, call_id, error } => Self::ToolCallError { tool_name, call_id, error },
            crate::client::engine::EngineEvent::ThinkingChunk { content } => Self::StreamThinkingChunk { content },
            crate::client::engine::EngineEvent::ThinkingComplete { content } => Self::StreamThinkingComplete { content },
        }
    }
}

/// Produce a human-readable label for a tool call from its result.
/// Returns e.g. "read: Read `path/to/file`" or "write: Write `path/to/file`"
pub fn tool_call_header(tool_name: &str, result: &str) -> String {
    let actual_result = if result.starts_with('{') && result.contains("\"result\"") {
        if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(result) {
            wrapper.get("result").map(|v| v.to_string()).unwrap_or(result.to_string())
        } else {
            result.to_string()
        }
    } else {
        result.to_string()
    };

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&actual_result) {
        if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
            if json.get("content").is_some() {
                return format!("{}: Read {}", tool_name, path);
            }
            if json.get("bytes_written").is_some() {
                return format!("{}: Write {}", tool_name, path);
            }
            if json.get("entries").is_some() {
                return format!("{}: List {}", tool_name, path);
            }
            if json.get("deleted").is_some() {
                return format!("{}: Delete {}", tool_name, path);
            }
            if json.get("created").is_some() {
                return format!("{}: Mkdir {}", tool_name, path);
            }
            if json.get("bytes_appended").is_some() {
                return format!("{}: Append {}", tool_name, path);
            }
            if json.get("lines_changed").is_some() {
                return format!("{}: Edit {}", tool_name, path);
            }
            if json.get("size").is_some() || json.get("is_file").is_some() || json.get("is_dir").is_some() {
                return format!("{}: Stat {}", tool_name, path);
            }
        }
        if let (Some(expr), Some(_result)) = (
            json.get("expression").and_then(|v| v.as_str()),
            json.get("result"),
        ) {
            return format!("{}: Calc {}", tool_name, expr);
        }
        if let Some(query) = json.get("query").and_then(|v| v.as_str()) {
            return format!("{}: Search {}", tool_name, query);
        }
        if let Some(results) = json.get("results").and_then(|v| v.as_array()) {
            return format!("{}: Search results ({} found)", tool_name, results.len());
        }
        if let (Some(target), Some(task)) = (
            json.get("target").and_then(|v| v.as_str()),
            json.get("task").and_then(|v| v.as_str()),
        ) {
            let short_task = if task.len() > 40 { &task[..40] } else { task };
            return format!("{}: Agent {} -> {}", tool_name, target, short_task);
        }
    }
    if actual_result.starts_with("Error:") || actual_result.starts_with("error:") {
        let truncated = if actual_result.len() > 50 { &actual_result[..50] } else { &actual_result };
        return format!("{}: {}", tool_name, truncated);
    }
    tool_name.to_string()
}

/// Format the current local time as HH:MM:SS.
pub fn format_timestamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}
