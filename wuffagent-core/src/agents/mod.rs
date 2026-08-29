//! Agent system: types, traits, config, registry, engine, and execution.
//!
//! Modules:
//! - `types` — AgentId, AgentType, AgentMetadata, AgentResult
//! - `traits` — AgentInvocation trait, AgentError
//! - `config` — AgentConfig, ShellConfig, WorkerConfig
//! - `invocation_registry` — AgentInvocationRegistry for inter-agent calls
//! - `registry` — AgentRegistry: loads and routes to agents
//! - `engine` — AgentEngine: top-level execution orchestrator
//! - `agent` — Agent: configurable LLM loop with tool calls
//!
//! LlmClient and ChatClientAdapter are re-exported from the top-level `llm` module.

pub mod types;
pub mod traits;
pub mod config;
pub mod invocation_registry;
pub mod registry;
pub mod engine;
pub mod agent;

pub use types::*;
pub use traits::*;
pub use config::*;
pub use super::llm::{ChatClientAdapter, LlmClient};
pub use invocation_registry::AgentInvocationRegistry;
pub use registry::AgentRegistry;
pub use engine::AgentEngine;
pub use agent::Agent;
