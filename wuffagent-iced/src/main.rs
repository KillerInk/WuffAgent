use std::sync::{Arc, Mutex};

use wuffagent_core::{
    agents::{AgentEngine, AgentRegistry},
    client::ChatClient,
    config::Config,
    server::ServerManager,
    tools::{builtin, registry::ToolRegistry, ToolManager, TracingToolLogger},
};

fn bootstrap() -> (
    Arc<Mutex<Config>>,
    Arc<ServerManager>,
    Arc<Mutex<ChatClient>>,
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

    let sessions_dir = wuffagent_core::sessions::sessions_dir(&config_path);
    {
        let mut cfg = config.lock().unwrap();
        cfg.sessions_dir = sessions_dir.clone();
        if cfg.session_id.is_none() {
            let session = wuffagent_core::sessions::create_session(&sessions_dir, "Untitled");
            cfg.session_id = Some(session.id.clone());
        } else if !wuffagent_core::sessions::session_exists(&sessions_dir, cfg.session_id.as_ref().unwrap()) {
            tracing::warn!(
                "Session file missing for id={}, creating new session",
                cfg.session_id.as_ref().unwrap()
            );
            let session = wuffagent_core::sessions::create_session(&sessions_dir, "Untitled");
            cfg.session_id = Some(session.id.clone());
        }
    }

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

    let logger = Arc::new(TracingToolLogger);
    let discovery_paths: Vec<std::path::PathBuf> = vec![
        dirs::config_dir().map(|d| d.join("wuffagent").join("plugins")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let registry = Arc::new(ToolRegistry::new(discovery_paths, logger));
    let invocation_registry = Arc::new(wuffagent_core::agents::invocation_registry::AgentInvocationRegistry::new());
    builtin::register_builtins(&registry, &invocation_registry).expect("Failed to register built-in tools");
    if let Err(e) = registry.discover_plugins() {
        eprintln!("Warning: failed to discover plugins: {}", e);
    }

    let tool_manager = Arc::new(ToolManager::new(registry.clone()));

    let base_url;
    let api_key;
    {
        let cfg = config.lock().unwrap();
        base_url = cfg.base_url();
        api_key = cfg.remote_api_key.clone();
    }
    let client = Arc::new(Mutex::new(ChatClient::new(&base_url)));

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

    {
        let mut cl = client.lock().unwrap();
        let _ = cl.load_session();
    }

    let base_url_clone = base_url.clone();
    let llm_client = Arc::new(wuffagent_core::agents::llm_client::ChatClientAdapter::new(
        ChatClient::new(&base_url_clone),
    ));

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

    let agent_engine = AgentEngine::new(
        agent_registry.clone(),
        llm_client,
        tool_manager.clone(),
        5,
    );
    let agent_engine = Arc::new(agent_engine);

    (config, server, client, tool_manager, agent_engine)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use wuffagent_iced_app::app::backend::Backend;
    use wuffagent_iced_app::subscription;

    let (config, server, client, tool_manager, agent_engine) = bootstrap();

    let runtime = Arc::new(tokio::runtime::Handle::current());
    let backend = Arc::new(Backend::new(
        runtime,
        client,
        server,
        config,
        tool_manager,
        agent_engine,
    ));

    subscription::set_event_sender(backend.event_sender.clone());

    let settings = iced::Settings {
        default_text_size: 14.0.into(),
        antialiasing: false,
        ..iced::Settings::default()
    };

    iced::application(
        move || wuffagent_iced_app::boot(backend.clone()),
        wuffagent_iced_app::update,
        wuffagent_iced_app::view,
    )
    .subscription(|_state| subscription::event_subscription())
    .settings(settings)
    .run()?;

    Ok(())
}
