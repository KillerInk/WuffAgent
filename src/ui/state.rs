use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::ChatClient;
use crate::config::Config;
use crate::server::ServerManager;
use crate::tools::ToolManager;

use crate::types::{AppEvent, AppStatus, ChatMessage};

// Re-export EngineEvent for use in other modules
pub use crate::client::engine::EngineEvent;

// Re-export agent types for use in other modules
pub use crate::agents::AgentPipeline;

/// A single task progress entry in the pipeline panel.
#[derive(Clone, Debug)]
pub struct PipelineTaskEntry {
    pub id: String,
    pub description: String,
    pub status: PipelineTaskStatus,
    pub agent_type: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PipelineTaskStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for PipelineTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineTaskStatus::Pending => write!(f, "pending"),
            PipelineTaskStatus::Running => write!(f, "running"),
            PipelineTaskStatus::Completed => write!(f, "completed"),
            PipelineTaskStatus::Failed => write!(f, "failed"),
        }
    }
}

/// State of the agent pipeline panel.
#[derive(Clone, Debug, Default)]
pub struct PipelineState {
    pub(super) active: bool,
    pub(super) plan_id: String,
    pub(super) iteration: u32,
    pub(super) tasks: Vec<PipelineTaskEntry>,
    pub(super) feedback_state: String,
    pub(super) cancelled: bool,
}

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
    /// Whether to scroll to bottom on the next frame (set when new message arrives while at bottom)
    pub(super) scroll_to_bottom_requested: bool,
    /// Whether the scroll-to-bottom button should be visible
    pub(super) button_visible: bool,
    /// Fade animation for the button (0.0 to 1.0)
    pub(super) button_opacity: f32,
    /// Whether user is currently at the bottom of the chat
    pub(super) at_bottom: bool,
    /// Current scroll offset Y position (tracked across frames)
    pub(super) scroll_offset_y: f32,
    /// Previous frame's scroll offset Y position
    pub(super) prev_scroll_offset_y: f32,
    /// Previous frame's content height (for computing was_at_bottom)
    pub(super) prev_content_height: f32,
    pub(super) pending_image: Option<String>,
    pub(super) editing_message_index: Option<usize>,
    pub(super) editing_message_content: String,
    pub(super) pending_error: Option<String>,
    pub(super) streaming_task: Option<JoinHandle<()>>,
    pub(super) engine: Option<crate::client::engine::ChatEngine>,
    /// Agent pipeline panel state
    pub(super) pipeline: PipelineState,
}

/// Session-related state extracted from ChatApp
pub struct SessionState {
    pub(super) sessions_panel: Option<super::sessions_panel::SessionsPanel>,
    pub(super) save_failure_message: Option<String>,
    pub(super) max_display_messages: usize,
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
    // UI flags
    pub(super) show_settings: bool,
    pub(super) settings_dialog: Option<super::settings::SettingsDialog>,
    pub(super) presets_dialog: Option<super::presets_dialog::PresetsDialog>,

    // Remote server state
    pub(super) remote_n_ctx: u32,
    pub(super) remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,
    pub(super) remote_n_ctx_handle: Option<JoinHandle<()>>,

    // UI progress
    pub(super) progress: f32,

    // Agent pipeline (optional, initialized if /plan trigger is used)
    pub(super) agent_pipeline: Option<Arc<AgentPipeline<ChatClient>>>,
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
        drop(cfg);
        let (tx, rx) = mpsc::channel();

        // Connect the UI event channel to the client's tool event sender
        // so that ToolCallStart/ToolCallComplete events reach the UI
        client.lock().unwrap().set_tool_event_sender(tx.clone());

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
                scroll_to_bottom_requested: false,
                button_visible: false,
                button_opacity: 0.0,
                at_bottom: true,
                scroll_offset_y: 0.0,
                prev_scroll_offset_y: 0.0,
                prev_content_height: 0.0,
                pending_image: None,
                editing_message_index: None,
                editing_message_content: String::new(),
                pending_error: None,
                streaming_task: None,
                engine: None,
                pipeline: PipelineState::default(),
            },
            sessions: SessionState {
                sessions_panel: Some(sessions_panel),
                save_failure_message: None,
                max_display_messages: max_messages,
            },
            show_settings: false,
            settings_dialog: None,
            presets_dialog: None,
            remote_n_ctx: 0,
            remote_n_ctx_arc: None,
            remote_n_ctx_handle: None,
            progress: 0.0,
            agent_pipeline: None,
        }
    }
}
