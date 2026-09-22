//! Agent system: types, traits, config, engine, and execution.
//!
//! Modules:
//! - `types` — AgentId
//! - `traits` — AgentError
//! - `config` — AgentConfig, ShellConfig, WorkerConfig
//! - `engine` — AgentEngine: top-level execution orchestrator
//! - `agent` — Agent: configurable LLM loop with tool calls
//!
//! LlmClient and ChatClientAdapter are re-exported from the top-level `llm` module.

pub mod agent;
pub mod chat_pipeline;
pub mod config;
pub mod engine;
pub mod improvement;
pub mod manager;
pub mod traits;
pub mod types;

pub use super::llm::{ChatClientAdapter, LlmClient};
pub use agent::Agent;
pub use chat_pipeline::ChatPipeline;
pub use config::*;
pub use engine::AgentEngine;
pub use improvement::suggest_improvements;
pub use traits::*;
pub use types::*;
