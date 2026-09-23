//! Base types brick: dependency-free shared types.
//!
//! Split into one file per family (message, usage, chat_ui, policy, events,
//! reasoning, timestamp); all items are re-exported here so
//! `crate::types::X` paths are unchanged for callers.
//!
//! External exception (the one documented egui image path, removed by the
//! deferred data-URI migration): `egui::ImageSource` in
//! `QueuedMessage.image`, plus `image.rs::image_source_data_uri`, which
//! converts it to the `data:` URI form used in model requests.

mod chat_ui;
mod events;
mod image;
mod message;
mod policy;
mod reasoning;
mod timestamp;
mod usage;

pub use chat_ui::{AppStatus, ChatMessage, MessageKind};
pub use events::AppEvent;
pub use image::image_source_data_uri;
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
