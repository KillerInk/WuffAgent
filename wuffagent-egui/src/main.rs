use std::sync::{Arc, Mutex};

use tracing_subscriber::prelude::*;

use wuffagent_core::{
    agents::AgentEngine,
    client::{ChatClient, ConnectionSettings},
    config::Config,
    server::ServerManager,
    sessions::sessions_dir,
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

mod image_loader;
mod ui;

/// A `FormatEvent` wrapper that silently drops egui-winit's "arboard paste
/// error" line: expected noise, since egui-winit only supports pasting TEXT,
/// so every Ctrl/Cmd+V with an image-only clipboard logs it even though our
/// own image-paste handler (ui/input.rs) reads the clipboard fine. All other
/// events are delegated to the default formatter unchanged.
///
/// (A `filter_fn` can't do this: it only sees event *metadata*, not the
/// message body; the clipboard target also logs genuinely useful errors like
/// "arboard copy/cut error" and "Failed to initialize arboard clipboard".)
type DefaultFmt = tracing_subscriber::fmt::format::Format<
    tracing_subscriber::fmt::format::Full,
    tracing_subscriber::fmt::time::SystemTime,
>;

struct QuietClipboardPaste(DefaultFmt);

/// Captures the value of the `message` field of an event (used to tell the
/// noisy "arboard paste error" line apart from the genuinely useful
/// copy/cut errors logged from the same target).
struct MessageCapture(Option<String>);

impl tracing::field::Visit for MessageCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = Some(value.to_string());
        }
    }
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for QuietClipboardPaste
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let is_expected_paste_noise = {
            if event.metadata().target() != "egui_winit::clipboard" {
                false
            } else {
                let mut capture = MessageCapture(None);
                event.record(&mut capture);
                capture
                    .0
                    .as_deref()
                    .is_some_and(|m| m.starts_with("arboard paste error"))
            }
        };
        if is_expected_paste_noise {
            return Ok(());
        }
        self.0.format_event(ctx, writer, event)
    }
}

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
    Arc<ConnectionSettings>,
    Arc<wuffagent_core::memory::MemoryManager>,
    Arc<wuffagent_core::tools::McpManager>,
) {
    // Default to `debug` for app crates, but silence the extremely chatty
    // `naga` WGSL shader compiler (pulled in by wgpu/egui) whose DEBUG-level
    // overload-resolution traces flood the console at startup. Users can still
    // override the whole filter via RUST_LOG.
    //
    // The fmt layer additionally drops egui-winit's "arboard paste error"
    // line: it is expected noise. egui-winit only supports pasting TEXT, so
    // every Ctrl/Cmd+V with an image-only clipboard logs it, even though our
    // own image-paste handler (ui/input.rs) reads the clipboard fine.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug,naga=off")),
        )
        .with(
            tracing_subscriber::fmt::layer().event_format(QuietClipboardPaste(Default::default())),
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

    // Single shared connection settings: every client (bootstrap engine,
    // per-session clients, non-streaming LLM adapter) reads its URL + key
    // from here, so a settings/preset change propagates to all of them with
    // one `update()` call (previously the app had to push `set_url` to every
    // live client and stale clones could still go out of sync).
    let base_url = config.base_url();
    let connection = Arc::new(ConnectionSettings::new(&base_url, config.remote_api_key.as_deref()));

    // Session-specific client for the bootstrap engine
    let mut client = ChatClient::from_settings((*connection).clone());
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
    let mut non_streaming = ChatClient::from_settings((*connection).clone());
    non_streaming.set_reasoning_effort(config.reasoning_effort);
    non_streaming.set_n_ctx(config.n_ctx);
    let llm_client = Arc::new(wuffagent_core::llm::ChatClientAdapter::new(non_streaming));
    let client_for_engine = Arc::new(client.clone());

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
    let mut memory_llm = ChatClient::from_settings_with_timeout((*connection).clone(), MEMORY_LLM_TIMEOUT_SECS);
    memory_llm.set_reasoning_effort(config.reasoning_effort);
    memory_llm.set_n_ctx(config.n_ctx);
    let memory_llm_client = Arc::new(wuffagent_core::llm::ChatClientAdapter::new(memory_llm));

    builtin::register_builtins(&registry, &config.search_config).expect("Failed to register built-in tools");
    if let Err(e) = registry.discover_plugins() {
        eprintln!("Warning: failed to discover plugins: {}", e);
    }

    let tool_manager: Arc<ToolManager> = Arc::new(ToolManager::new(registry.clone()));

    // MCP (Model Context Protocol) manager. It owns a DEDICATED 2-worker
    // tokio runtime on which all MCP I/O runs (the UI thread is inside the
    // main runtime, where block_on is not allowed). Connected servers' tools
    // are mirrored into the shared registry as `mcp__<server>__<tool>`, so
    // ToolManager per-agent rebuilds pick them up automatically.
    let mcp_manager = Arc::new(wuffagent_core::tools::McpManager::new(registry.clone()));
    mcp_manager.sync_from_config(&config.mcp_servers);
    // Auto-connect enabled servers (fire-and-forget on the MCP runtime;
    // failures are logged and retryable from the MCP panel).
    mcp_manager.auto_connect_enabled();

    let server = ServerManager::new(
        &config.server_path,
        &config.model_path,
        config.port,
        config.n_gpu_layers,
        config.n_ctx,
        config.threads,
    );
    let tool_manager_for_engine: Arc<Mutex<ToolManager>> = Arc::new(Mutex::new((*tool_manager).clone()));

    // Initialize memory manager (with the memory-dedicated LLM client that
    // has the longer total timeout — see MEMORY_LLM_TIMEOUT_SECS above).
    let memory_config = config.memory_config.clone();
    let memory_llm_client_clone = memory_llm_client.clone();
    let memory_manager = wuffagent_core::memory::MemoryManager::new_with_llm(memory_config, memory_llm_client_clone)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to initialize memory manager with LLM: {}, falling back", e);
            wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default())
                .unwrap_or_else(|_| wuffagent_core::memory::MemoryManager::new(wuffagent_core::memory::MemoryConfig::default()).unwrap())
        });
    let memory_manager = Arc::new(memory_manager);

    // Register memory tools with the memory manager
    builtin::register_memory_tools(&registry, memory_manager.clone()).expect("Failed to register memory tools");

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
    let mut agents_search_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        agents_search_dirs.push(cwd.join("agents"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            agents_search_dirs.push(exe_dir.join("agents"));
        }
    }

    let agent_engine = AgentEngine::new(
        llm_client,
        tool_manager_for_engine,
        client_for_engine,
    )
    .with_memory(memory_manager.clone())
    .with_agents_dir(agents_dir)
    .with_agents_search_dirs(agents_search_dirs);

    let agent_engine = Arc::new(agent_engine);

    (config, server, tool_manager, agent_engine, connection, memory_manager, mcp_manager)
}

#[tokio::main]
async fn main() -> eframe::Result {
    let (config, server, tool_manager, agent_engine, connection, memory_manager, mcp_manager) = bootstrap();

    // Dedicated runtime for UI-triggered async work (memory maintenance).
    // NOTE: the UI thread (main thread) IS inside the `#[tokio::main]` runtime
    // context — the async `main` body blocks in `eframe::run_native`, which
    // runs inside `rt.block_on`, and that context is also what lets the UI
    // loop `tokio::spawn` (remote n_ctx fetch). Because of it, no
    // `Runtime::block_on` may be called from the UI thread at all (it would
    // panic with "Cannot start a runtime from within a runtime"); the memory
    // panel therefore runs the maintenance pass on a helper thread that
    // `block_on`s this dedicated runtime, keeping it separate from the main
    // runtime.
    // Worker threads are capped well below `num_cpus` since only the
    // occasional maintenance pass runs here; the default would spin up one
    // idle thread per core.
    let memory_runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to create memory runtime"),
    );
    
    // Initialize session store with the configured session
    // Shared event channel: the UI polls the receiver each frame; the pipeline
    // (and client tool events) write into the sender. Created before the
    // initial runtime so it can be shared with the first session.
    let (event_tx, event_rx) = std::sync::mpsc::channel::<wuffagent_core::types::AppEvent>();

    let mut session_store: std::collections::HashMap<String, wuffagent_core::sessions::SessionRuntime> = std::collections::HashMap::new();
    let mut selected_session_id: Option<String> = None;

    if let Some(session_id) = &config.session_id {
        if wuffagent_core::sessions::session_exists(&config.sessions_dir, session_id) {
            let mut runtime = wuffagent_core::sessions::SessionRuntime::create_from_config(
                &config,
                &connection,
                &agent_engine,
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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WuffAgent (egui)",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_fonts(emoji_fonts());
            // egui ships no image decoders: register the app's loader so
            // `ImageSource::Bytes` (pasted/attached images, chat-bubble
            // images) renders instead of the red "no image loaders are
            // loaded" error texture.
            cc.egui_ctx.add_image_loader(Arc::new(image_loader::ImageBytesLoader));
            Ok(Box::new(ui::state::ChatApp::new(
                config, server, tool_manager, agent_engine, connection,
                session_store, selected_session_id, event_tx, event_rx,
                memory_manager, memory_runtime, mcp_manager,
            )))
        }),
    )
}
