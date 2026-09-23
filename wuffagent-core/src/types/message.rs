//! Chat message wire types: `Message` (with multimodal content handling),
//! `ToolCall`, `ToolFunction`.

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

/// The function specification within a tool call.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}
