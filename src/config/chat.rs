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
        }
    }
}

/// Chat settings extracted from Config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChatSettings {
    pub system_prompt: String,
    pub streaming: bool,
    pub theme: String,
    #[serde(default)]
    pub auto_scroll: bool,
    pub chat_history: Vec<ChatMessage>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
}

fn default_max_messages() -> usize {
    100
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            streaming: true,
            theme: "dark".to_string(),
            auto_scroll: true,
            chat_history: Vec::new(),
            session_id: None,
            max_messages: 100,
        }
    }
}
