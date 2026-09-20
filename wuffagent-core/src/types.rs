use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A chat message with a role (system/user/assistant/tool) and content.
///
/// `image` holds an attached image as a `data:` URI (e.g.
/// `data:image/png;base64,...`). It is NOT stored as a separate JSON field:
/// on the wire (and in session files) an image message serializes its
/// `content` as the OpenAI multimodal parts array
/// (`[{"type":"text",...},{"type":"image_url",...}]`) so vision-capable
/// servers receive the standard format, and deserialization rebuilds
/// `content` (joined text parts) + `image` (first `image_url`) from it.
/// Messages without an image keep the plain string `content`.
// Serialize/Deserialize are implemented manually below (see `MessageDe`):
// `content` may be a plain string or a multimodal parts array, and serde
// derive cannot express that.
#[derive(Clone, Debug)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    /// Tool call requests from the AI (non-null when the AI wants to invoke a tool).
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Reference to the tool call this result belongs to (for tool role messages).
    pub tool_call_id: Option<String>,
    /// Model reasoning/thinking content (llama.cpp `reasoning_content`, DeepSeek/Qwen style).
    /// Round-tripped so the model can see its own prior reasoning across tool-call rounds.
    pub reasoning_content: Option<String>,
    /// Attached image as a `data:` URI (user messages with an image).
    /// Serialized inside `content` as an `image_url` part (see struct docs).
    pub image: Option<String>,
}

/// Wire form of [`Message`] where `content` stays raw so it can be either a
/// plain string (no image) or a JSON array of multimodal content parts.
#[derive(Deserialize)]
struct MessageDe {
    role: String,
    content: serde_json::Value,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Split the raw content value into plain text + attached image.
/// Accepts both the legacy plain-string form and the multimodal parts
/// array, so old session files keep loading unchanged.
fn split_content(content: serde_json::Value) -> (String, Option<String>) {
    match content {
            serde_json::Value::String(s) => (s, None),
            serde_json::Value::Array(parts) => {
                let mut text = String::new();
                let mut image = None;
                for part in parts {
                    match part.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                        Some("image_url") => {
                            if image.is_none() {
                                image = part
                                    .get("image_url")
                                    .and_then(|u| u.get("url"))
                                    .and_then(|u| u.as_str())
                                    .map(|s| s.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                (text, image)
            }
            // Null or any other shape — keep it loadable, display as empty.
            _ => (String::new(), None),
        }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let de = MessageDe::deserialize(deserializer)?;
        let (content, image) = split_content(de.content);
        Ok(Message {
            role: de.role,
            content,
            timestamp: de.timestamp,
            tool_calls: de.tool_calls,
            tool_call_id: de.tool_call_id,
            reasoning_content: de.reasoning_content,
            image: image,
        })
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Message", 6)?;
        state.serialize_field("role", &self.role)?;
        if let Some(ref url) = self.image {
            // Multimodal content parts (OpenAI/llama.cpp vision format).
            let mut parts = Vec::new();
            if !self.content.is_empty() {
                parts.push(serde_json::json!({ "type": "text", "text": self.content }));
            }
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": url },
            }));
            state.serialize_field("content", &parts)?;
        } else {
            state.serialize_field("content", &self.content)?;
        }
        state.serialize_field("timestamp", &self.timestamp)?;
        if let Some(ref tool_calls) = self.tool_calls {
            state.serialize_field("tool_calls", tool_calls)?;
        }
        if let Some(ref tool_call_id) = self.tool_call_id {
            state.serialize_field("tool_call_id", tool_call_id)?;
        }
        if let Some(ref reasoning_content) = self.reasoning_content {
            state.serialize_field("reasoning_content", reasoning_content)?;
        }
        state.end()
    }
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
    /// Remote server n_ctx was updated.
    NCtxUpdated { n_ctx: u32, session_id: String },
    /// Agent self-improvement suggestions generated.
    ImprovementSuggested {
        agent_name: String,
        suggestions: Vec<crate::memory::ImprovementSuggestion>,
        session_id: String,
    },
    /// The session switched agents: the running agent called the `handoff`
    /// tool and the target agent now continues the same conversation.
    AgentHandoff {
        from: String,
        to: String,
        task: String,
        session_id: String,
    },
    /// The agent asked to restart the WuffAgent process (optionally after a
    /// build). The UI saves the session, writes a restart marker, relaunches
    /// the (optionally newly built) binary, and closes the window; the new
    /// process resumes this session automatically.
    RestartRequested {
        reason: String,
        build_cmd: Option<String>,
        exe_path: Option<String>,
        session_id: String,
    },
}

/// Format the current time as a human-readable timestamp string.
///
/// Includes the local date (`YYYY-MM-DD HH:MM:SS`) so the UI can draw day
/// separators. Display-only field — never parsed by the model layer. Legacy
/// sessions stored the older 8-char `HH:MM:SS` form; see `timestamp_day` /
/// `timestamp_time` for tolerant parsing.
pub fn format_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The `YYYY-MM-DD` day part of a display timestamp, if it has one.
/// Returns `None` for legacy time-only timestamps (no separator is drawn).
pub fn timestamp_day(ts: &str) -> Option<&str> {
    if ts.len() >= 11 && ts.as_bytes()[4] == b'-' {
        Some(&ts[..10])
    } else {
        None
    }
}

/// The time part of a display timestamp for rendering, or the whole string
/// for legacy time-only timestamps.
pub fn timestamp_time(ts: &str) -> &str {
    if ts.len() >= 19 && ts.as_bytes()[10] == b' ' {
        &ts[11..]
    } else {
        ts
    }
}

/// Format a tool call header for display.
pub fn tool_call_header(name: &str, result: &str) -> String {
    format!("🔧 {}: {}", name, result.chars().take(80).collect::<String>())
}

#[cfg(test)]
mod tests;
