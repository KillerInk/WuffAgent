//! Shared configuration types re-exported for use across modules.
//!
//! This module centralizes configuration structs that are used by both
//! `config` and `memory`/`agents` to avoid circular dependencies.

pub use crate::agents::config::{AgentConfig, RecoveryPolicy, ShellConfig, WorkerConfig};
pub use crate::memory::types::{InjectionMode, MemoryConfig, MemoryEntry, MemoryType, SearchMode};
