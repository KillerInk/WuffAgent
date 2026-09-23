//! WuffAgent core library.
//!
//! Module dependency graph:
//!
//! ```text
//! lib
//! ├── types          (base types: Message, AppEvent, QueuedMessage, ChatToolPolicy)
//! │                  → no internal dependencies
//! ├── llm            (LlmClient trait, ChatClientAdapter) → client, types
//! ├── client         (ChatClient: HTTP/SSE streaming) → types, trimming, usage,
//! │                  sessions (session persistence fns), tools (ToolDefinition
//! │                  in request types)
//! ├── config         (Config, ConnectionType) → re-exports agents::config,
//! │                  memory::types
//! ├── config_types   (shared re-exports) → agents::config, memory::types
//! ├── agents         (Agent, AgentEngine, ChatPipeline) → llm, client, tools,
//! │                  memory, trimming, sessions (QueuedMessage), types
//! │   ├── agent      (Agent: execute, loop, prompt, verify, toolcall_parse,
//! │   │               tool_exec, tool_calls, inject, memory_sync, stats)
//! │   ├── config     (AgentConfig, ShellConfig, WorkerConfig,
//! │   │               profiles, load)
//! │   ├── types      (AgentId, RunStats, Handoff/RestartRequest)
//! │   ├── traits     (AgentError)
//! │   ├── engine
//! │   ├── manager
//! │   ├── improvement
//! │   └── chat_pipeline
//! ├── tools          (ToolManager, ToolRegistry) → agents, memory, config, types
//! │   ├── builtin
//! │   ├── dynamic
//! │   └── preview
//! ├── sessions       (Session model, runtime) → agents (AgentEngine), types
//! ├── memory         (MemoryManager) → llm, agents, types, config (one
//! │                  home-dir call)
//! ├── usage          (token-usage log + aggregation) → config
//! ├── trimming       (conversation trimming) → tools, types
//! └── server         (HTTP server) → no internal dependencies
//! ```
//!
//! Key invariants:
//! - `types` has no dependencies at all — not internal, not external.
//!   Images cross the core boundary as `data:` URI strings
//!   (`QueuedMessage.image`, `Message.image`); the egui layer converts the
//!   attached `ImageSource` to that form before a message enters core.
//! - One documented residual 3-cycle: `client → sessions → agents → client`.
//!   Each edge is justified: client persists sessions via
//!   `client/session.rs` free fns; `sessions/runtime.rs` drives
//!   `AgentEngine`; the streaming tool path (`agents/agent/loop.rs`) needs
//!   `ChatClient` directly. No 2-cycles exist between top-level modules.
//! - `client` never depends on `agents` (ChatPipeline lives in
//!   `agents::chat_pipeline`); only `agents` may depend on `client`.
//! - `config` re-exports from `agents::config` and `memory::types`;
//!   `config_types` mirrors those re-exports for backward compatibility.
//! - `usage` depends only on `config`, so `client → usage` creates no cycle.

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
pub use agents::{AgentEngine, ChatPipeline};
pub use client::ChatClient;
pub use config::Config;
pub use server::ServerManager;
pub use tools::ToolManager;
pub use types::{AppEvent, AppStatus, ChatMessage, MessageKind, ReasoningEffort};
