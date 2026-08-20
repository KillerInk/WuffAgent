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
