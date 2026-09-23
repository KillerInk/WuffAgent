//! Reasoning effort level sent to the model server.

use serde::{Deserialize, Serialize};

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
