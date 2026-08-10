use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::ChatClient;
use crate::config::{Config, ConnectionType};
use crate::server::ServerManager;
use crate::tools::ToolManager;

use crate::types::{AppEvent, AppStatus, ChatMessage};

// Re-export EngineEvent for use in other modules
pub use crate::client::engine::EngineEvent;

/// Chat-related state extracted from ChatApp
#[derive(Default)]
pub struct ChatState {
    pub(super) messages: Vec<ChatMessage>,
    pub(super) input_text: String,
    pub(super) is_generating: bool,
    pub(super) status: AppStatus,
    pub(super) streaming: bool,
    pub(super) current_response: String,
    pub(super) token_count: u32,
    pub(super) context_used: f32,
    pub(super) auto_scroll: bool,
    pub(super) pending_image: Option<String>,
    pub(super) editing_message_index: Option<usize>,
    pub(super) editing_message_content: String,
    pub(super) pending_error: Option<String>,
    pub(super) streaming_task: Option<JoinHandle<()>>,
    pub(super) engine: Option<crate::client::engine::ChatEngine>,
}

/// Session-related state extracted from ChatApp
pub struct SessionState {
    pub(super) sessions_panel: Option<super::sessions_panel::SessionsPanel>,
    pub(super) save_failure_message: Option<String>,
    pub(super) max_display_messages: usize,
}

/// Settings-related state extracted from ChatApp
pub struct SettingsState {
    pub(super) connection_type: ConnectionType,
    pub(super) remote_url: String,
    pub(super) remote_api_key: String,
    pub(super) server_path: String,
    pub(super) model_path: String,
    pub(super) port: u16,
    pub(super) n_gpu_layers: i32,
    pub(super) n_ctx: u32,
    pub(super) threads: u32,
    pub(super) max_messages: usize,
    pub(super) system_prompt: String,
    pub(super) streaming: bool,
    pub(super) auto_scroll: bool,
    pub(super) theme: String,
}

/// Main app state with extracted sub-structs
pub struct ChatApp {
    // Core dependencies
    pub(super) server: Arc<ServerManager>,
    pub(super) client: Arc<Mutex<ChatClient>>,
    pub(super) config: Arc<Mutex<Config>>,
    pub(super) tool_manager: Arc<ToolManager>,
    pub(super) pending_tx: Option<mpsc::Sender<AppEvent>>,
    pub(super) pending_rx: Mutex<mpsc::Receiver<AppEvent>>,

    // Extracted state structs
    pub(super) chat: ChatState,
    pub(super) sessions: SessionState,
    pub(super) settings: Option<SettingsState>,

    // UI flags
    pub(super) show_settings: bool,
    pub(super) settings_dialog: Option<super::settings::SettingsDialog>,

    // Remote server state
    pub(super) remote_n_ctx: u32,
    pub(super) remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,
    pub(super) remote_n_ctx_handle: Option<JoinHandle<()>>,

    // UI progress
    pub(super) progress: f32,
}

impl ChatApp {
    pub fn new(
        server: Arc<ServerManager>,
        client: Arc<Mutex<ChatClient>>,
        config: Arc<Mutex<Config>>,
        tool_manager: Arc<ToolManager>,
    ) -> Self {
        let cfg = config.lock().unwrap();
        let streaming = cfg.streaming;
        let max_messages = cfg.max_messages;
        let auto_scroll = cfg.auto_scroll;
        drop(cfg);
        let (tx, rx) = mpsc::channel();

        // Initialize sessions panel and load the current session
        let sessions_panel = super::sessions_panel::SessionsPanel::new(&config.clone());
        let mut messages: Vec<ChatMessage> = Vec::new();
        {
            let mut cl = client.lock().unwrap();
            if let Some(session) = cl.load_session() {
                messages = session.messages.iter().map(|m| ChatMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                    timestamp: m.timestamp.clone(),
                    image: None,
                }).collect();
            }
        }

        Self {
            server,
            client,
            config: config.clone(),
            tool_manager,
            pending_tx: Some(tx),
            pending_rx: Mutex::new(rx),
            chat: ChatState {
                messages,
                input_text: String::new(),
                is_generating: false,
                status: AppStatus::Stopped,
                streaming,
                current_response: String::new(),
                token_count: 0,
                context_used: 0.0,
                auto_scroll,
                pending_image: None,
                editing_message_index: None,
                editing_message_content: String::new(),
                pending_error: None,
                streaming_task: None,
                engine: None,
            },
            sessions: SessionState {
                sessions_panel: Some(sessions_panel),
                save_failure_message: None,
                max_display_messages: max_messages,
            },
            settings: None,
            show_settings: false,
            settings_dialog: None,
            remote_n_ctx: 0,
            remote_n_ctx_arc: None,
            remote_n_ctx_handle: None,
            progress: 0.0,
        }
    }
}
