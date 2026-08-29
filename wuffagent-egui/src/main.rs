use std::sync::{Arc, Mutex};

use wuffagent_core::{
    agents::{AgentEngine, AgentRegistry},
    client::ChatClient,
    config::Config,
    server::ServerManager,
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
    ChatClient,
    Arc<ToolManager>,
    Arc<AgentEngine>,
) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
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

    let sessions_dir = wuffagent_core::sessions::sessions_dir(&config_path);
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

    let is_remote = config.is_remote();

    let _server = if is_remote {
        ServerManager::noop()
    } else {
        ServerManager::new(
            &config.server_path,
            &config.model_path,
            config.port,
            config.n_gpu_layers,
            config.n_ctx,
            config.threads,
        )
    };

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

    let base_url_clone = base_url.clone();
    let llm_client = Arc::new(wuffagent_core::llm::ChatClientAdapter::new(
        ChatClient::new(&base_url_clone),
    ));
    let client_for_engine = Arc::new(client.clone());

    let config_path_clone = config_path.clone();
    let config_agents_dir = config_path_clone
        .parent()
        .map(|p| p.join("agents"))
        .unwrap_or_else(|| config_path_clone.clone());

    let mut search_dirs = vec![config_agents_dir.clone()];
    let mut add_agents_dir = |path: std::path::PathBuf| {
        if path.exists() {
            search_dirs.push(path);
        }
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            add_agents_dir(exe_dir.parent().map(|p| p.join("agents")).unwrap_or_default());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        add_agents_dir(cwd.join("agents"));
    }

    // Load agents FIRST, then build the invocation registry, then register builtins
    // so that agent_call resolves against a populated registry.
    let agent_registry = match AgentRegistry::load(search_dirs.clone(), &registry) {
        Ok(reg) => {
            tracing::info!("Loaded {} agents from {:?}", reg.agent_count(), search_dirs);
            reg
        }
        Err(e) => {
            tracing::warn!("Failed to load agents: {}, using empty registry", e);
            AgentRegistry::default()
        }
    };
    let agent_registry = Arc::new(agent_registry);

    // Build the shared invocation registry now that agents are loaded
    let invocation_registry = agent_registry.build_invocation_registry(
        llm_client.clone(),
        Arc::new(Mutex::new(ToolManager::new(registry.clone()))),
        client_for_engine.clone(),
    );

    // Register builtins â€” agent_call will use the populated registry
    builtin::register_builtins(&registry, &invocation_registry).expect("Failed to register built-in tools");
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

    // Initialize memory manager with LLM client for extraction and improvement
    let memory_config = config.memory_config.clone();
    let llm_client_clone = llm_client.clone();
    let memory_manager = wuffagent_core::memory::MemoryManager::new_with_llm(memory_config, llm_client_clone)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to initialize memory manager with LLM: {}, falling back", e);
            wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default())
                .unwrap_or_else(|_| wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default()).unwrap())
        });
    let memory_manager = Arc::new(memory_manager);

    let mut agent_engine = AgentEngine::new(
        agent_registry.clone(),
        llm_client,
        tool_manager_for_engine,
        client_for_engine,
        invocation_registry,
    ).with_memory(memory_manager);

    // Load the most recent agent session for the selected agent so the engine
    // starts with conversation context from a previous session.
    if let Some(agent_name) = agent_registry.agent_names().first() {
        let agent_dir = wuffagent_core::sessions::sessions_dir(&config_path)
            .join("agents")
            .join(agent_name);
        if agent_dir.exists() {
            let sessions = wuffagent_core::sessions::list_sessions(&agent_dir);
            if let Some(latest) = sessions.first() {
                tracing::info!(
                    "Loading most recent agent session for '{}': {} ({} messages)",
                    agent_name,
                    latest.id,
                    latest.messages.len()
                );
                agent_engine.set_agent_session(Some(latest.id.clone()), agent_dir.clone());
            }
        }
    }

    let agent_engine = Arc::new(agent_engine);

    (config, server, client, tool_manager, agent_engine)
}

#[tokio::main]
async fn main() -> eframe::Result {
    let (config, server, client, tool_manager, agent_engine) = bootstrap();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WuffAgent (egui)",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_fonts(emoji_fonts());
            Ok(Box::new(ui::state::ChatApp::new(
                config, client, server, tool_manager, agent_engine,
            )))
        }),
    )
}
