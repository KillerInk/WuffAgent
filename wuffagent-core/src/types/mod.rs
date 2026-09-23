//! Base types brick: dependency-free shared types (no internal modules and
//! no external UI-framework types — images cross the core boundary as
//! `data:` URI strings).
//!
//! Split into one file per family (message, usage, chat_ui, policy, events,
//! reasoning, timestamp); all items are re-exported here so
//! `crate::types::X` paths are unchanged for callers.

mod chat_ui;
mod events;
mod message;
mod policy;
mod reasoning;
mod timestamp;
mod usage;

pub use chat_ui::{AppStatus, ChatMessage, MessageKind};
pub use events::AppEvent;
pub use message::{Message, ToolCall, ToolFunction};
pub use policy::{
    ChatToolPolicy, ImprovementSuggestion, NewAgentProposal, QueuedMessage, ShellConfig,
    TrimConfig,
};
pub use reasoning::ReasoningEffort;
pub use timestamp::{format_timestamp, timestamp_day, timestamp_time};
pub use usage::{LlamaTimings, PromptProgress, Usage};

#[cfg(test)]
mod tests;
