use std::sync::{Arc, Mutex};

use wuffagent_core::{
    agents::AgentEngine,
    client::ChatClient,
    config::Config,
    server::ServerManager,
    sessions::{SessionRuntime, sessions_dir},
    tools::{builtin, registry::ToolRegistry, ToolManager, TracingToolLogger},
};

// Re-export core modules so `mod ui` can use `crate::...` paths
pub use wuffagent_core::types;
pub use wuffagent_core::client;
pub use wuffagent_core::config;
pub use wuffagent_core::server;
pub use wuffagent_core::sessions;
pub use wuffagent_core::tools;
pub use wuffagent_core::agents;
pub use wuffagent_core::memory;

mod ui;

fn emoji_fonts() -> egui::FontDefinitions {
    let mut font_data = egui::FontDefinitions::default();
    // Load system emoji font for Windows
    #[cfg(target_os = "windows")]
    {
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguiemj.ttf") {
            font_data
                .font_data
                .insert("emoji".to_string(), Arc::new(egui::FontData::from_owned(data)));
            font_data
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "emoji".to_string());
        }
    }
    // Load system emoji font for macOS
    #[cfg(target_os = "macos")]
    {
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc") {
            font_data
                .font_data
                .insert("emoji".to_string(), Arc::new(egui::FontData::from_owned(data)));
            font_data
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "emoji".to_string());
        }
    }
    font_data
}

fn bootstrap() -> (
    Config,
    ServerManager,
    Arc<ToolManager>,
    Arc<AgentEngine>,
    ChatClient,
    ChatClient,
    Arc<wuffagent_core::memory::MemoryManager>,
) {
    // Default to `debug` for app crates, but silence the extremely chatty
    // `naga` WGSL shader compiler (pulled in by wgpu/egui) whose DEBUG-level
    // overload-resolution traces flood the console at startup. Users can still
    // override the whole filter via RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug,naga=off")),
        )
        .init();

    let config_path = wuffagent_core::config::get_config_path();
    let mut config = match Config::load(&config_path) {
        Ok(mut cfg) => {
            cfg.file_path = config_path.clone();
            cfg
        }
        Err(e) => {
            eprintln!("Failed to load config: {}, using defaults", e);
            Config { file_path: config_path.clone(), ..Default::default() }
        }
    };

    let sessions_dir = sessions_dir(&config_path);
    {
        config.sessions_dir = sessions_dir.clone();
        if config.session_id.is_none() {
            let session = wuffagent_core::sessions::create_session(&sessions_dir, "Untitled");
            config.session_id = Some(session.id.clone());
        } else if !wuffagent_core::sessions::session_exists(&sessions_dir, config.session_id.as_ref().unwrap()) {
            tracing::warn!(
                "Session file missing for id={}, creating new session",
                config.session_id.as_ref().unwrap()
            );
            let session = wuffagent_core::sessions::create_session(&sessions_dir, "Untitled");
            config.session_id = Some(session.id.clone());
        }
    }

    let logger = Arc::new(TracingToolLogger);
    let discovery_paths: Vec<std::path::PathBuf> = vec![
        dirs::config_dir().map(|d| d.join("wuffagent").join("plugins")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let registry = Arc::new(ToolRegistry::new(discovery_paths, logger));

    // Resolve search dirs for agent discovery
    let base_url = config.base_url();
    let api_key = config.remote_api_key.clone();
    let mut client = ChatClient::new(&base_url);
    client.set_api_key(api_key.as_deref());
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
    // Clone for the UI so a preset/settings URL change can be pushed to the
    // bootstrap engine's own client (used before per-session engines exist) in
    // place. Shares the same interior-mutable base_url as `client_for_engine`.
    let bootstrap_stream_client = client.clone();

    // Non-streaming LlmClient for agents: configure auth + request options
    // BEFORE wrapping in the adapter (the adapter only holds the client handle).
    let base_url_clone = base_url.clone();
    let mut non_streaming = ChatClient::new(&base_url_clone);
    non_streaming.set_api_key(api_key.as_deref());
    non_streaming.set_reasoning_effort(config.reasoning_effort);
    non_streaming.set_n_ctx(config.n_ctx);
    // Keep a clone for the UI: it shares the same interior-mutable base_url as
    // the adapter's client, so the app can push a preset/settings URL change
    // to every non-streaming call path in place.
    let bootstrap_llm_client = non_streaming.clone();
    let llm_client = Arc::new(wuffagent_core::llm::ChatClientAdapter::new(non_streaming));
    let client_for_engine = Arc::new(client.clone());

    builtin::register_builtins(&registry).expect("Failed to register built-in tools");
    if let Err(e) = registry.discover_plugins() {
        eprintln!("Warning: failed to discover plugins: {}", e);
    }

    let tool_manager: Arc<ToolManager> = Arc::new(ToolManager::new(registry.clone()));

    let server = ServerManager::new(
        &config.server_path,
        &config.model_path,
        config.port,
        config.n_gpu_layers,
        config.n_ctx,
        config.threads,
    );
    let tool_manager_for_engine: Arc<Mutex<ToolManager>> = Arc::new(Mutex::new((*tool_manager).clone()));

    // Initialize memory manager
    let memory_config = config.memory_config.clone();
    let llm_client_clone = llm_client.clone();
    let memory_manager = wuffagent_core::memory::MemoryManager::new_with_llm(memory_config, llm_client_clone)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to initialize memory manager with LLM: {}, falling back", e);
            wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default())
                .unwrap_or_else(|_| wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default()).unwrap())
        });
    let memory_manager = Arc::new(memory_manager);

    // Register memory tools with the memory manager
    builtin::register_memory_tools(&registry, memory_manager.clone()).expect("Failed to register memory tools");

    let agent_engine = AgentEngine::new(
        llm_client,
        tool_manager_for_engine,
        client_for_engine,
    ).with_memory(memory_manager.clone());

    let agent_engine = Arc::new(agent_engine);

    (config, server, tool_manager, agent_engine, bootstrap_llm_client, bootstrap_stream_client, memory_manager)
}

#[tokio::main]
async fn main() -> eframe::Result {
    let (config, server, tool_manager, agent_engine, bootstrap_llm_client, bootstrap_stream_client, memory_manager) = bootstrap();

    // Dedicated runtime for UI-triggered async work (memory maintenance) so the
    // UI thread can block on a blocking_call without touching the main runtime
    // that the chat pipeline is spawned on.
    let memory_runtime = tokio::runtime::Runtime::new().expect("failed to create memory runtime");
    
    // Initialize session store with the configured session
    // Shared event channel: the UI polls the receiver each frame; the pipeline
    // (and client tool events) write into the sender. Created before the
    // initial runtime so it can be shared with the first session.
    let (event_tx, event_rx) = std::sync::mpsc::channel::<wuffagent_core::types::AppEvent>();

    let mut session_store: std::collections::HashMap<String, wuffagent_core::sessions::SessionRuntime> = std::collections::HashMap::new();
    let mut selected_session_id: Option<String> = None;

    if let Some(session_id) = &config.session_id {
        if let Some(session) = wuffagent_core::sessions::load_session(&config.sessions_dir, session_id) {
            // Create a fresh client for this session
            let base_url = config.base_url();
            let mut session_client = ChatClient::new(&base_url);
            session_client.set_api_key(config.remote_api_key.as_deref());
            session_client.set_session(Some(session_id.clone()), config.sessions_dir.clone());
            session_client.set_reasoning_effort(config.reasoning_effort);
            session_client.set_max_messages(config.max_messages);
            session_client.set_n_ctx(config.n_ctx);
            if config.encryption_enabled {
                if let Some(key) = config.encryption_key() {
                    session_client.set_encryption_key(Some(key));
                }
            }
            let _ = session_client.load_session();

            // Route tool-call events into the shared channel.
            session_client.set_tool_event_sender(event_tx.clone());

            // Per-session engine: bound to this session's client so the agent
            // chat loop reads/writes an isolated conversation store (the shared
            // bootstrap engine's client would otherwise be mutated by every
            // session in parallel â€” a cross-session data race).
            let session_engine = (*agent_engine)
                .clone()
                .with_client(session_client.clone())
                .with_session_id(session_id.clone());
            // Create pipeline (carries the session_id for event routing) and runtime.
            let pipeline = wuffagent_core::client::ChatPipeline::new(
                Arc::new(session_engine.clone()),
                event_tx.clone(),
                config.reasoning_effort,
                session_id.clone(),
            );
            let cancel_token = tokio_util::sync::CancellationToken::new();

            let mut runtime = SessionRuntime::new(
                session_id.clone(),
                session.name.clone(),
                session_client,
                pipeline,
                session_engine,
                cancel_token,
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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WuffAgent (egui)",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_fonts(emoji_fonts());
            Ok(Box::new(ui::state::ChatApp::new(
                config, server, tool_manager, agent_engine, bootstrap_llm_client,
                bootstrap_stream_client, session_store, selected_session_id, event_tx, event_rx,
                memory_manager, memory_runtime,
            )))
        }),
    )
}
