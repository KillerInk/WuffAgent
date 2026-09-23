//! Reasoning effort level sent to the model server.

use serde::{Deserialize, Serialize};

/// Reasoning effort level sent to the model server (Qwen3/llama.cpp style).
///
/// Wire semantics (see [`ReasoningEffort::as_wire_value`] /
/// [`ReasoningEffort::enable_thinking`] and `client::http::reasoning_wire`):
/// every level is made EXPLICIT on the wire so the toggle works regardless of
/// the backend's default. Qwen3.x (e.g. Qwen3.8) defaults to thinking ON at
/// `xhigh` and has no `reasoning_effort` off-level (only xhigh/medium/low), so
/// `Off` disables thinking via the Qwen3 chat-template kwarg
/// `chat_template_kwargs: {enable_thinking: false}`; on-levels send
/// `reasoning_effort` plus `enable_thinking: true` (which also switches
/// thinking ON on backends that default it off, e.g. llama.cpp Qwen3).
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

    /// Whether the Qwen3-style `chat_template_kwargs.enable_thinking` should
    /// be `true` (thinking on) or `false` (thinking off) on the wire. Sent
    /// with every request so the on/off state does not depend on the
    /// backend's default; ignored by backends whose templates lack the kwarg.
    pub fn enable_thinking(self) -> bool {
        matches!(self, ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High)
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
