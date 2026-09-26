//! App bootstrap: build everything the UI app needs, once, at startup.
//!
//! Stages (order matters — later stages depend on earlier output):
//! 1. [`init tracing`](crate::logging::init_tracing)
//! 2. [`load_config`] — config.json + the active session (created/repaired)
//! 3. [`build_clients`] — shared connection settings + the three chat clients
//! 4. [`build_tooling`] — tool registry, builtins, plugins, MCP
//! 5. memory manager + memory/skill tools
//! 6. agents discovery + agent-profile/plugin tools
//! 7. the shared [`AgentEngine`]
//!
//! [`initial_session_store`] (called from `main`) then materializes the
//! first `SessionRuntime` for the configured session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use wuffagent_core::{
    agents::AgentEngine,
    client::{ChatClient, ConnectionSettings},
    config::Config,
    llm::ChatClientAdapter,
    memory::MemoryManager,
    server::ServerManager,
    sessions::{self, sessions_dir, SessionRuntime},
    tools::{builtin, registry::ToolRegistry, McpManager, ToolManager, TracingToolLogger},
    types::AppEvent,
};

/// Everything the UI app needs, built once at startup.
///
/// Named fields replace the old 9-element bootstrap tuple: every consumer
/// reads what it needs by name, and adding a field can't silently rotate
/// positional arguments.
pub struct AppContext {
    pub config: Config,
    pub server: ServerManager,
    pub tool_manager: Arc<ToolManager>,
    pub agent_engine: Arc<AgentEngine>,
    pub connection: Arc<ConnectionSettings>,
    pub memory_manager: Arc<MemoryManager>,
    pub mcp_manager: Arc<McpManager>,
    pub event_tx: std::sync::mpsc::Sender<AppEvent>,
    pub event_rx: std::sync::mpsc::Receiver<AppEvent>,
}

/// The three chat clients the app runs: the streaming session client
/// (engine + session runtime), the non-streaming LLM adapter (agents), and
/// the memory-dedicated LLM adapter (long timeout).
struct ClientSet {
    session_client: Arc<ChatClient>,
    llm_client: Arc<ChatClientAdapter>,
    memory_llm_client: Arc<ChatClientAdapter>,
}

/// Registry + MCP + all tool registrations (builtins, plugins, MCP,
/// memory/skill/agent/plugin tools are registered by the caller on this).
struct Tooling {
    registry: Arc<ToolRegistry>,
    tool_manager: Arc<ToolManager>,
    mcp_manager: Arc<McpManager>,
}

/// Build the full app context (call once, from `main`).
pub fn bootstrap() -> AppContext {
    crate::logging::init_tracing();

    let config = load_config();
    let connection = Arc::new(ConnectionSettings::new(
        &config.base_url(),
        config.remote_api_key.as_deref(),
    ));
    let clients = build_clients(&config, &connection);
    let tooling = build_tooling(&config);

    // Shared event channel: the UI polls the receiver each frame; the pipeline
    // (and client tool events) write into the sender.
    let (event_tx, event_rx) = std::sync::mpsc::channel::<AppEvent>();

    // Register MCP management tools (`mcp_list` / `mcp_add_server` /
    // `mcp_connect` / `mcp_disconnect` / `mcp_remove_server` /
    // `mcp_refresh_tools` / `mcp_set_tool_enabled`) — bound to the app's
    // McpManager; shared-registry tools gated by `allowed_tools`.
    // The event sender lets the add/remove tools emit McpConfigChanged so the
    // UI reloads config.json (session_id=None: app-level registration, the
    // run-specific session is resolved by the UI when the event arrives).
    let mcp_event_tx = Arc::new(Mutex::new(event_tx.clone()));
    builtin::register_mcp_tools(&tooling.registry, tooling.mcp_manager.clone(), Some(mcp_event_tx), None)
        .expect("Failed to register MCP management tools");

    let server = ServerManager::new(
        &config.server_path,
        &config.model_path,
        config.port,
        config.n_gpu_layers,
        config.n_ctx,
        config.threads,
    );

    // Initialize memory manager (with the memory-dedicated LLM client that
    // has the longer total timeout — see build_clients).
    let memory_manager = wuffagent_core::memory::MemoryManager::new_with_llm(
        config.memory_config.clone(),
        clients.memory_llm_client.clone(),
    )
    .unwrap_or_else(|e| {
        tracing::warn!("Failed to initialize memory manager with LLM: {}, falling back", e);
        wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default())
            .unwrap_or_else(|_| {
                wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default()).unwrap()
            })
    });
    let memory_manager = Arc::new(memory_manager);

    // Register memory tools with the memory manager
    builtin::register_memory_tools(&tooling.registry, memory_manager.clone())
        .expect("Failed to register memory tools");

    // K1: skill (procedural memory) tools — backed by `<wuffagent_home>/skills/`.
    // Shared-registry tools; per-profile visibility is gated by `allowed_tools`.
    let skill_store = Arc::new(wuffagent_core::memory::skills::SkillStore::default());
    builtin::register_skill_tools(&tooling.registry, skill_store).expect("Failed to register skill tools");

    // Agents directory (the `agents/` subdirectory next to the config file) —
    // the chat path resolves `handoff` targets from here (same directory the
    // UI's agent selector scans).
    let agents_dir = config
        .file_path
        .parent()
        .map(|p| p.join("agents"))
        .unwrap_or_else(|| config.file_path.clone());

    // Project-level agents dirs for handoff target discovery — MUST mirror the
    // UI's agent selector (window.rs / improvements.rs): without these, the
    // chat path's `handoff` tool only sees the config-dir `agents/` and
    // reports "Available agents: none" when profiles live in the project.
    let mut agents_search_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        agents_search_dirs.push(cwd.join("agents"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            agents_search_dirs.push(exe_dir.join("agents"));
        }
    }

    // T1: agent-profile self-modification tools (`list_agents` /
    // `edit_agent_profile`) — bound to the same discovery set as the UI's
    // agent selector (primary + project-level search dirs). Shared-registry
    // tools; per-profile visibility is gated by `allowed_tools` like any
    // other tool.
    {
        let mut agent_manager = wuffagent_core::agents::manager::AgentManager::new(agents_dir.clone());
        for dir in &agents_search_dirs {
            agent_manager.add_search_dir(dir.clone());
        }
        builtin::register_agent_tools(&tooling.registry, Arc::new(agent_manager))
            .expect("Failed to register agent-profile tools");
    }

    // T3b: runtime plugin management tools (`reload_plugins` /
    // `add_plugin_path`) — bound to the shared registry itself, so an agent
    // can load a native plugin into the very registry it reads from (new
    // tools visible from the next message). Shared-registry tools gated by
    // `allowed_tools` like any other tool.
    builtin::register_plugin_tools(&tooling.registry, tooling.registry.clone())
        .expect("Failed to register plugin management tools");

    let agent_engine = AgentEngine::new(
        clients.llm_client,
        Arc::new(Mutex::new((*tooling.tool_manager).clone())),
        clients.session_client,
    )
    .with_memory(memory_manager.clone())
    .with_agents_dir(agents_dir)
    .with_agents_search_dirs(agents_search_dirs);

    AppContext {
        config,
        server,
        tool_manager: tooling.tool_manager,
        agent_engine: Arc::new(agent_engine),
        connection,
        memory_manager,
        mcp_manager: tooling.mcp_manager,
        event_tx,
        event_rx,
    }
}

/// Load config.json (falling back to defaults) and make sure the active
/// session exists on disk (creating a fresh one when the configured id is
/// missing or the file was deleted).
fn load_config() -> Config {
    let config_path = wuffagent_core::config::get_config_path();
    let mut config = match Config::load(&config_path) {
        Ok(mut cfg) => {
            cfg.file_path = config_path.clone();
            cfg
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load config, using defaults");
            Config { file_path: config_path.clone(), ..Default::default() }
        }
    };

    let dir = sessions_dir(&config_path);
    {
        config.sessions_dir = dir.clone();
        if config.session_id.is_none() {
            let session = sessions::create_session(&dir, "Untitled");
            config.session_id = Some(session.id.clone());
        } else if !sessions::session_exists(&dir, config.session_id.as_ref().unwrap()) {
            tracing::warn!(
                "Session file missing for id={}, creating new session",
                config.session_id.as_ref().unwrap()
            );
            let session = sessions::create_session(&dir, "Untitled");
            config.session_id = Some(session.id.clone());
        }
    }
    config
}

/// Build the shared connection settings and the three chat clients.
///
/// Single shared connection settings: every client reads its URL + key from
/// here, so a settings/preset change propagates to all of them with one
/// `update()` call (previously the app had to push `set_url` to every live
/// client and stale clones could still go out of sync).
fn build_clients(config: &Config, connection: &ConnectionSettings) -> ClientSet {
    // Session-specific client for the bootstrap engine
    let mut client = ChatClient::from_settings(connection.clone());
    client.set_session(config.session_id.clone(), config.sessions_dir.clone());
    client.set_reasoning_effort(config.reasoning_effort);
    client.set_max_messages(config.max_messages);
    client.set_n_ctx(config.n_ctx);
    if config.encryption_enabled {
        if let Some(key) = config.encryption_key() {
            client.set_encryption_key(Some(key));
        }
    }
    let _ = client.load_session();

    // Non-streaming LlmClient for agents: configure request options BEFORE
    // wrapping in the adapter (the adapter only holds the client handle).
    let mut non_streaming = ChatClient::from_settings(connection.clone());
    non_streaming.set_reasoning_effort(config.reasoning_effort);
    non_streaming.set_n_ctx(config.n_ctx);
    let llm_client = Arc::new(ChatClientAdapter::new(non_streaming));
    let session_client = Arc::new(client);

    // Dedicated non-streaming client for the memory subsystem (maintenance,
    // auto-improve). Maintenance runs the store in small batches
    // (`memory_maintenance_batch_size`), one non-streaming prompt each; a
    // single batch on a local model can still take several minutes — far
    // beyond the regular 300 s total timeout (the cause of the "error sending
    // request for url .../v1/chat/completions" failures after 5 min). The
    // actual give-up point is the user-configurable
    // `memory_maintenance_timeout_secs`, now applied PER STEP via
    // tokio::time::timeout in both the panel (around the whole batched pass)
    // and the post-task engine path (around a single step); this 3600 s
    // client timeout (= the config's maximum) is only a safety net against a
    // hung server, so the configured value is always the effective bound.
    const MEMORY_LLM_TIMEOUT_SECS: u64 = 3600;
    let mut memory_llm =
        ChatClient::from_settings_with_timeout(connection.clone(), MEMORY_LLM_TIMEOUT_SECS);
    memory_llm.set_reasoning_effort(config.reasoning_effort);
    memory_llm.set_n_ctx(config.n_ctx);
    let memory_llm_client = Arc::new(ChatClientAdapter::new(memory_llm));

    ClientSet {
        session_client,
        llm_client,
        memory_llm_client,
    }
}

/// Build the tool registry, register builtins, discover plugins, and set up
/// the MCP manager (auto-connecting enabled servers). MCP *management*
/// tools and the memory/skill/agent/plugin tools are registered by the
/// caller (they need the event channel / managers built alongside).
fn build_tooling(config: &Config) -> Tooling {
    let logger = Arc::new(TracingToolLogger);
    // Default plugin discovery dir: `<config dir>/plugins` — i.e.
    // `~/.wuffagent/plugins/`, next to `agents/` and `sessions/` (the
    // config dir is the directory holding config.json).
    let discovery_paths: Vec<PathBuf> = vec![config.file_path.parent().map(|p| p.join("plugins"))]
        .into_iter()
        .flatten()
        .collect();

    let registry = Arc::new(ToolRegistry::new(discovery_paths, logger));

    builtin::register_builtins(&registry, &config.search_config).expect("Failed to register built-in tools");
    if let Err(e) = registry.discover_plugins() {
        tracing::warn!(error = %e, "Failed to discover plugins");
    }

    let tool_manager = Arc::new(ToolManager::new(registry.clone()));

    // MCP (Model Context Protocol) manager. It owns a DEDICATED 2-worker
    // tokio runtime on which all MCP I/O runs (the UI thread is inside the
    // main runtime, where block_on is not allowed). Connected servers' tools
    // are mirrored into the shared registry as `mcp__<server>__<tool>`, so
    // ToolManager per-agent rebuilds pick them up automatically.
    let mcp_manager = Arc::new(McpManager::new(registry.clone()));
    mcp_manager.sync_from_config(&config.mcp_servers);
    // Auto-connect enabled servers (fire-and-forget on the MCP runtime;
    // failures are logged and retryable from the MCP panel).
    mcp_manager.auto_connect_enabled();

    Tooling {
        registry,
        tool_manager,
        mcp_manager,
    }
}

/// Materialize the initial `SessionRuntime` for the configured session (if
/// the session file exists), populating its chat state from the loaded
/// conversation so the restored history is visible on startup.
///
/// Returns the (possibly empty) store plus the selected session id.
pub fn initial_session_store(
    config: &Config,
    connection: &Arc<ConnectionSettings>,
    agent_engine: &Arc<AgentEngine>,
    event_tx: &std::sync::mpsc::Sender<AppEvent>,
) -> (HashMap<String, SessionRuntime>, Option<String>) {
    let mut session_store: HashMap<String, SessionRuntime> = HashMap::new();
    let mut selected_session_id: Option<String> = None;

    if let Some(session_id) = &config.session_id {
        if sessions::session_exists(&config.sessions_dir, session_id) {
            let mut runtime = SessionRuntime::create_from_config(
                config,
                connection,
                agent_engine,
                session_id.clone(),
                "Untitled".to_string(),
                event_tx.clone(),
            );

            // Populate the chat display from the loaded conversation so the
            // restored session's history is visible on startup.
            {
                let conv = runtime.client.conversation().clone();
                runtime.chat_state.reload_messages_from_client(&conv);
            }

            session_store.insert(session_id.clone(), runtime);
            selected_session_id = Some(session_id.clone());
        }
    }

    (session_store, selected_session_id)
}
