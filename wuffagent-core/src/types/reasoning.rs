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

/// How the chat input selects a reasoning level for a session.
///
/// - `Auto` (default): use the reasoning effort configured on the selected
///   agent profile (`AgentConfig.reasoning_effort`). The profile's own value
///   is applied by `Agent::new`, and the client's forced level is reset to
///   `Off` so nothing else leaks in.
/// - `Explicit(e)`: force level `e` for the session, overriding whatever the
///   agent profile says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasoningMode {
    #[default]
    Auto,
    Explicit(ReasoningEffort),
}

impl ReasoningMode {
    /// Whether the mode follows the agent profile's own effort.
    pub fn is_auto(self) -> bool {
        matches!(self, ReasoningMode::Auto)
    }

    /// The forced level, or `None` in Auto mode.
    pub fn explicit(self) -> Option<ReasoningEffort> {
        match self {
            ReasoningMode::Auto => None,
            ReasoningMode::Explicit(e) => Some(e),
        }
    }

    /// Human-readable label for the UI dropdown.
    pub fn label(self) -> &'static str {
        match self {
            ReasoningMode::Auto => "Auto",
            ReasoningMode::Explicit(e) => e.label(),
        }
    }
}

// Serde: a plain string ("auto" | "off" | "low" | "medium" | "high") so the
// session file stays readable and backward-compatible (missing field ->
// `ReasoningMode::default()` = Auto).
impl Serialize for ReasoningMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ReasoningMode::Auto => serializer.serialize_str("auto"),
            ReasoningMode::Explicit(e) => {
                let s = e.name().to_lowercase();
                serializer.serialize_str(&s)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ReasoningMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "auto" => Ok(ReasoningMode::Auto),
            _ => Ok(ReasoningMode::Explicit(
                serde_json::from_value(serde_json::Value::String(s))
                    .map_err(serde::de::Error::custom)?,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_mode_roundtrip() {
        for mode in [
            ReasoningMode::Auto,
            ReasoningMode::Explicit(ReasoningEffort::Off),
            ReasoningMode::Explicit(ReasoningEffort::Low),
            ReasoningMode::Explicit(ReasoningEffort::Medium),
            ReasoningMode::Explicit(ReasoningEffort::High),
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: ReasoningMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn reasoning_mode_serializes_as_plain_string() {
        assert_eq!(serde_json::to_string(&ReasoningMode::Auto).unwrap(), "\"auto\"");
        assert_eq!(
            serde_json::to_string(&ReasoningMode::Explicit(ReasoningEffort::High)).unwrap(),
            "\"high\""
        );
    }
}
