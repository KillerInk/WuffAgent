//! WuffAgent core library.
//!
//! Module dependency graph:
//!
//! ```text
//! lib
//! ├── types          (base types: Message, AppEvent)
//! ├── llm            (LlmClient trait, ChatClientAdapter) → uses client, types
//! ├── client         (ChatClient: HTTP/SSE streaming) → uses types
//! ├── config         (Config, ConnectionType) → re-exports agents::config, memory::types
//! ├── config_types   (shared re-exports) → uses agents::config, memory::types
//! ├── agents         (Agent, AgentEngine) → uses llm, client, tools, types, sessions, memory
//! │   ├── config     (AgentConfig, ShellConfig, WorkerConfig)
//! │   ├── types      (AgentId)
//! │   ├── traits     (AgentError)
//! │   ├── engine
//! │   └── agent
//! ├── tools          (ToolManager, ToolRegistry) → uses types
//! │   ├── builtin
//! │   └── dynamic
//! ├── sessions       (Session model)
//! ├── memory         (MemoryManager) → uses llm, types
//! ├── usage          (token-usage log + aggregation) → uses config, types
//! └── trimming       (conversation trimming)
//!     ├── classifier
//!     ├── config
//!     └── summarizer
//! ```
//!
//! Key invariants:
//! - `types` has no internal dependencies.
//! - `llm` depends on `client` and `types`.
//! - `config` re-exports from `agents::config` and `memory::types`.
//! - `config_types` mirrors `config` re-exports for backward compatibility.
//! - `agents` depends on `llm`, `client`, `tools`, `types`, `sessions`, and `memory`.
//! - `memory` depends on `llm` and `types`.
//! - `client` depends on `usage` (one log line per completed LLM call);
//!   `usage` depends only on `config`/`types`, so no cycle.
//! - No circular dependencies exist between top-level modules.

pub mod agents;
pub mod client;
pub mod config;
pub mod config_types;
pub mod llm;
pub mod memory;
pub mod server;
pub mod sessions;
pub mod tools;
pub mod trimming;
pub mod types;
pub mod usage;

// Common UI-facing types re-exported for convenient access from egui consumers.
pub use agents::AgentEngine;
pub use client::ChatClient;
pub use config::Config;
pub use server::ServerManager;
pub use tools::ToolManager;
pub use types::{AppEvent, AppStatus, ChatMessage, MessageKind, ReasoningEffort};
