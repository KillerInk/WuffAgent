//! AI Memory System for WuffAgent
//!
//! Persistent, project-scoped memory that allows the AI to learn across sessions.
//! Supports storing facts, lessons, decisions, context, and goals.
//! Memories are written by agent tools (save_memory, update_memory, consolidate_memories)
//! and injected into agent system prompts.
//!
//! Module dependency: `memory` uses `llm` for improvement,
//! but `llm` does not depend on `memory`.

pub mod types;
pub mod storage;
pub mod search;
pub mod manager;
pub mod improver;

pub use types::{MemoryEntry, MemoryType, MemoryConfig, SearchMode, InjectionMode};
pub use manager::{MemoryManager, MemoryAddResult, MaintenanceReport, MaintenanceProgress};
pub use storage::{load_memories, save_memories, get_memories_path};
pub use improver::{ImprovementSuggestion, NewAgentProposal, suggest_improvements};
