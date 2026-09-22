use serde::{Deserialize, Serialize};

use crate::types::Message;

/// A chat message for persistence in config (alias for the shared Message type).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
}

impl From<Message> for ChatMessage {
    fn from(m: Message) -> Self {
        ChatMessage {
            role: m.role,
            content: m.content,
            timestamp: m.timestamp,
        }
    }
}

impl From<ChatMessage> for Message {
    fn from(m: ChatMessage) -> Self {
        Message {
            role: m.role,
            content: m.content,
            timestamp: m.timestamp,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }
    }
}
