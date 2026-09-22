//! AI Memory System for WuffAgent
//!
//! Persistent, project-scoped memory that allows the AI to learn across sessions.
//! Supports storing facts, lessons, decisions, context, and goals.
//! Memories are written by agent tools (save_memory, update_memory, consolidate_memories)
//! and injected into agent system prompts.
//!
//! Module dependency: `memory` uses `llm` for improvement,
//! but `llm` does not depend on `memory`.

pub mod improver;
pub mod manager;
pub mod search;
pub mod storage;
pub mod types;

pub use improver::{suggest_improvements, ImprovementSuggestion, NewAgentProposal};
pub use manager::{MaintenanceProgress, MaintenanceReport, MemoryAddResult, MemoryManager};
pub use storage::{get_memories_path, load_memories, save_memories};
pub use types::{InjectionMode, MemoryConfig, MemoryEntry, MemoryType, SearchMode};
