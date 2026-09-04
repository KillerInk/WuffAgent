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
            ReasoningEffort::High => Some("xhigh"),
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
    /// Display kind — replaces legacy string-prefix conventions (💭 prefix, "||" tool format).
    #[serde(default)]
    pub kind: MessageKind,
}

/// How a UI chat message should be rendered.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    #[default]
    Normal,
    /// Model reasoning/thinking (rendered dim + italic).
    Thinking,
    /// Tool call result; content is "header||call_id||result_json" or a bare result.
    Tool,
}

/// Events that flow from the client engine to the UI.
///
/// Each event carries a `session_id` so the UI can route it to the correct
/// session's chat area. When multiple sessions run in parallel, events must
/// not be mixed between sessions.
#[derive(Clone, Debug)]
pub enum AppEvent {
    StreamChunk { content: String, session_id: String },
    /// An intermediate tool round finished (text committed, generation continues).
    StreamRoundComplete { content: String, usage: Option<Usage>, session_id: String },
    StreamComplete { content: String, usage: Option<Usage>, session_id: String },
    StreamError { error: String, session_id: String },
    ToolCallWarning { tool_name: String, message: String, session_id: String },
    ToolCallStart { tool_name: String, call_id: String, session_id: String },
    ToolCallComplete { tool_name: String, call_id: String, result: String, session_id: String },
    ToolCallError { tool_name: String, call_id: String, error: String, session_id: String },
    // Thinking output events (e.g. Claude-style reasoning)
    StreamThinkingChunk { content: String, session_id: String },
    StreamThinkingComplete { content: String, session_id: String },
    // Agent chain events
    AgentChainStarted { agent_name: String, depth: u32, session_id: String },
    AgentChainCompleted { agent_name: String, result: String, depth: u32, session_id: String },
    AgentChainError { agent_name: String, error: String, depth: u32, session_id: String },
    AgentChainCancelled { agent_name: String, session_id: String },
    AgentChainComplete { response: String, entries: Vec<crate::sessions::model::AgentChainEntry>, session_id: String },
    AgentChainStopped { session_id: String },
    /// Remote server n_ctx was updated.
    NCtxUpdated { n_ctx: u32, session_id: String },
    /// Agent self-improvement suggestions generated.
    ImprovementSuggested {
        agent_name: String,
        suggestions: Vec<crate::memory::ImprovementSuggestion>,
        session_id: String,
    },
}

/// Format the current time as a human-readable timestamp string.
pub fn format_timestamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Format a tool call header for display.
pub fn tool_call_header(name: &str, result: &str) -> String {
    format!("🔧 {}: {}", name, result.chars().take(80).collect::<String>())
}
