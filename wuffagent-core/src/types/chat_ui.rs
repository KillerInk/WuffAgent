//! UI-facing chat types: app status, display messages, message kinds.

use serde::{Deserialize, Serialize};

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
    /// Tool call result; content is "header||call_id||result_json"
    /// (plus an optional "||duration_ms" fourth part for calls made while
    /// the live tool-card UI is active) or a bare result.
    Tool,
}
