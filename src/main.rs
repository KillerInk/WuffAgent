#[cfg(feature = "egui-ui")]
pub mod types;
#[cfg(feature = "egui-ui")]
pub mod client;
#[cfg(feature = "egui-ui")]
pub mod config;
#[cfg(feature = "egui-ui")]
pub mod server;
#[cfg(feature = "egui-ui")]
pub mod ui;
#[cfg(feature = "egui-ui")]
pub mod tools;
#[cfg(feature = "egui-ui")]
pub mod sessions;
#[cfg(feature = "egui-ui")]
pub mod agents;

#[cfg(feature = "iced-ui")]
pub mod types;
#[cfg(feature = "iced-ui")]
pub mod client;
#[cfg(feature = "iced-ui")]
pub mod config;
#[cfg(feature = "iced-ui")]
pub mod server;
#[cfg(feature = "iced-ui")]
pub mod tools;
#[cfg(feature = "iced-ui")]
pub mod sessions;
#[cfg(feature = "iced-ui")]
pub mod agents;
#[cfg(feature = "iced-ui")]
pub mod app;

use std::sync::{Arc, Mutex};

use client::ChatClient;
use config::Config;
use server::ServerManager;
use tools::{builtin, registry::ToolRegistry, ToolManager, TracingToolLogger};
use agents::{AgentRegistry, AgentEngine};

/// Bootstrap shared state and return a Backend (for iced) or the values needed for egui.
fn bootstrap() -> (
    Arc<Mutex<Config>>,
    Arc<ServerManager>,
    Arc<Mutex<ChatClient>>,
    Arc<ToolManager>,
    Arc<AgentEngine>,
) {
    // Initialize tracing subscriber for debug logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .init();

    // Load config
    let config_path = config::get_config_path();
    let config = match Config::load(&config_path) {
        Ok(mut cfg) => {
            cfg.file_path = config_path.clone();
            Arc::new(Mutex::new(cfg))
        }
        Err(e) => {
            eprintln!("Failed to load config: {}, using defaults", e);
            Arc::new(Mutex::new(Config { file_path: config_path.clone(), ..Default::default() }))
        }
    };

    // Setup sessions directory and ensure a session exists
    let sessions_dir = sessions::sessions_dir(&config_path);
    {
        let mut cfg = config.lock().unwrap();
        cfg.sessions_dir = sessions_dir.clone();
        if cfg.session_id.is_none() {
            let session = sessions::create_session(&sessions_dir, "Untitled");
            cfg.session_id = Some(session.id.clone());
        } else if !sessions::session_exists(&sessions_dir, cfg.session_id.as_ref().unwrap()) {
            tracing::warn!(
                "Session file missing for id={}, creating new session",
                cfg.session_id.as_ref().unwrap()
            );
            let session = sessions::create_session(&sessions_dir, "Untitled");
            cfg.session_id = Some(session.id.clone());
        }
    }

    // Create server manager: real instance for local mode, noop for remote
    let is_remote;
    {
        let cfg = config.lock().unwrap();
        is_remote = cfg.is_remote();
    }

    let server = if is_remote {
        Arc::new(ServerManager::noop())
    } else {
        let cfg = config.lock().unwrap();
        Arc::new(ServerManager::new(
            &cfg.server_path,
            &cfg.model_path,
            cfg.port,
            cfg.n_gpu_layers,
            cfg.n_ctx,
            cfg.threads,
        ))
    };

    // Initialize tool registry with builtins + dynamic plugin discovery
    let logger = Arc::new(TracingToolLogger);
    let discovery_paths: Vec<std::path::PathBuf> = vec![
        dirs::config_dir().map(|d| d.join("wuffagent").join("plugins")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let registry = Arc::new(ToolRegistry::new(discovery_paths, logger));

    // Register built-in tools
    let invocation_registry = Arc::new(crate::agents::invocation_registry::AgentInvocationRegistry::new());
    builtin::register_builtins(&registry, &invocation_registry).expect("Failed to register built-in tools");

    // Discover and load dynamic plugins
    if let Err(e) = registry.discover_plugins() {
        eprintln!("Warning: failed to discover plugins: {}", e);
    }

    let tool_manager = Arc::new(ToolManager::new(registry.clone()));

    // Create chat client using config's base_url()
    let base_url;
    let api_key;
    {
        let cfg = config.lock().unwrap();
        base_url = cfg.base_url();
        api_key = cfg.remote_api_key.clone();
    }
    let client = Arc::new(Mutex::new(ChatClient::new(&base_url)));

    // Set API key, session, and encryption key for the client
    {
        let mut cl = client.lock().unwrap();
        cl.set_api_key(api_key.as_deref());
        let cfg = config.lock().unwrap();
        cl.set_session(cfg.session_id.clone(), cfg.sessions_dir.clone());
        if cfg.encryption_enabled {
            if let Some(key) = cfg.encryption_key() {
                cl.set_encryption_key(Some(key));
            }
        }
    }

    // Load the active session into the client conversation
    {
        let mut cl = client.lock().unwrap();
        let _ = cl.load_session();
    }

    // Create LLM client adapter for agents
    let base_url_clone = base_url.clone();
    let llm_client = Arc::new(agents::llm_client::ChatClientAdapter::new(
        ChatClient::new(&base_url_clone),
    ));

    // Build search dirs for AgentRegistry: config agents/ + project workers/
    let config_path_clone = config_path.clone();
    let config_agents_dir = config_path_clone
        .parent()
        .map(|p| p.join("agents"))
        .unwrap_or_else(|| config_path_clone.clone());

    let mut search_dirs = vec![config_agents_dir.clone()];

    let mut add_workers_dir = |path: std::path::PathBuf| {
        if path.exists() {
            search_dirs.push(path);
        }
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            add_workers_dir(exe_dir.parent().map(|p| p.join("workers")).unwrap_or_default());
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        add_workers_dir(cwd.join("workers"));
    }

    // Create AgentRegistry
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

    // Create AgentEngine
    let agent_engine = AgentEngine::new(
        agent_registry.clone(),
        llm_client,
        tool_manager.clone(),
        5, // max_depth
    );
    let agent_engine = Arc::new(agent_engine);

    (config, server, client, tool_manager, agent_engine)
}

#[cfg(feature = "egui-ui")]
async fn run_egui(
    config: Arc<Mutex<Config>>,
    server: Arc<ServerManager>,
    client: Arc<Mutex<ChatClient>>,
    tool_manager: Arc<ToolManager>,
    agent_engine: Arc<AgentEngine>,
) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };

    eframe::run_native(
        "WuffAgent",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_fonts(egui::FontDefinitions::default());
            Ok(Box::new(ui::state::ChatApp::new(
                server,
                client,
                config,
                tool_manager,
                agent_engine,
            )))
        }),
    )
}

#[cfg(feature = "iced-ui")]
async fn run_iced(
    config: Arc<Mutex<Config>>,
    server: Arc<ServerManager>,
    client: Arc<Mutex<ChatClient>>,
    tool_manager: Arc<ToolManager>,
    agent_engine: Arc<AgentEngine>,
) -> Result<(), Box<dyn std::error::Error>> {
    use app::backend::Backend;

    let runtime = Arc::new(tokio::runtime::Handle::current());
    let backend = Arc::new(Backend::new(
        runtime,
        client,
        server,
        config,
        tool_manager,
        agent_engine,
    ));

    // Set up the event sender for iced subscriptions
    app::subscription::set_event_sender(backend.event_sender.clone());

    let settings = iced::Settings {
        default_text_size: 14.0.into(),
        antialiasing: false,
        ..iced::Settings::default()
    };

    iced::application(
        move || app::boot(backend.clone()),
        app::update,
        app::view,
    )
    .subscription(|_state| app::subscription::event_subscription())
    .settings(settings)
    .run()?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (config, server, client, tool_manager, agent_engine) = bootstrap();

    #[cfg(feature = "egui-ui")]
    {
        run_egui(config, server, client, tool_manager, agent_engine).await?;
    }

    #[cfg(feature = "iced-ui")]
    {
        run_iced(config, server, client, tool_manager, agent_engine).await?;
    }

    Ok(())
}
