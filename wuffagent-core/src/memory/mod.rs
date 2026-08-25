//! AI Memory System for WuffAgent
//!
//! Persistent, project-scoped memory that allows the AI to learn across sessions.
//! Supports storing facts, lessons, decisions, context, and goals.
//! Memories are injected into agent system prompts and can be auto-extracted from conversations.

pub mod types;
pub mod storage;
pub mod search;
pub mod manager;
pub mod extractor;

pub use types::{MemoryEntry, MemoryType, MemoryConfig, SearchMode, InjectionMode};
pub use manager::MemoryManager;
pub use storage::{load_memories, save_memories, get_memories_path};
pub use extractor::{extract_memories, build_extraction_prompt};
