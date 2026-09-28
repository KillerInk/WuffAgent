//! AI Memory System for WuffAgent
//!
//! Persistent, project-scoped memory that allows the AI to learn across sessions.
//! Supports storing facts, lessons, decisions, context, and goals.
//! Memories are written by agent tools (save_memory, update_memory, consolidate_memories)
//! and injected into agent system prompts.
//!
//! Module dependency: `memory` uses `llm` for improvement,
//! but `llm` does not depend on `memory`.

pub mod context;
pub mod evals;
pub mod manager;
pub mod maintenance;
pub mod pending_store;
pub mod search;
pub mod skills;
pub mod storage;
pub mod types;

// The suggestion LLM call lives in the agents brick; re-exported so the
// crate::memory:: paths stay stable (memory keeps the trigger in
// MemoryManager::suggest_improvements, which calls into it).
pub use crate::agents::improvement::suggest_improvements;
pub use crate::types::{ImprovementSuggestion, NewAgentProposal};
pub use maintenance::{MaintenanceProgress, MaintenanceReport};
pub use manager::{MemoryAddResult, MemoryManager};
pub use pending_store::PendingStore;
pub use skills::{
    build_skills_prompt_block, set_skills_dir_for_testing, Skill, SkillMeta, SkillStore,
};
pub use evals::{set_evals_dir_for_testing, Eval, EvalStore};
pub use storage::{get_memories_path, load_memories, save_memories};
pub use types::{
    AgentImprovementState, ImprovementStatus, InjectionMode, MemoryConfig, MemoryEntry,
    MemoryType, SearchMode,
};
