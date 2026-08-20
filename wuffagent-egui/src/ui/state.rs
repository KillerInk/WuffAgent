use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::client::ChatClient;
use crate::config::Config;
use crate::server::ServerManager;
use crate::tools::ToolManager;

use crate::types::{AppEvent, AppStatus, ChatMessage};

// Re-export EngineEvent for use in other modules
pub use crate::client::engine::EngineEvent;

/// A single task progress entry in the pipeline panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PipelineTaskStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct PipelineTaskEntry {
    pub id: String,
    pub description: String,
    pub status: PipelineTaskStatus,
    pub agent_type: String,
}

/// State for the agent chain panel.
#[derive(Clone, Debug, Default)]
pub struct AgentChainState {
    pub active: bool,
    pub entries: Vec<crate::sessions::model::AgentChainEntry>,
    pub current_agent: Option<String>,
    pub cancelled: bool,
}

/// State for the presets dialog.
#[derive(Clone, Debug)]
pub struct PresetsDialogState {
    pub show_presets: Arc<Mutex<bool>>,
}

/// Main application state for the egui UI.
pub struct ChatApp {
    pub config: Config,
    pub client: ChatClient,
    pub server: ServerManager,
    pub tool_manager: Arc<ToolManager>,
    pub agent_engine: Arc<crate::agents::AgentEngine>,
    pub cancellation_token: CancellationToken,
    pub chat: ChatAreaState,
    pub sessions: SessionsPanelState,
    pub status: AppStatus,
    pub show_settings: bool,
    pub settings_dialog: Option<super::settings::SettingsDialog>,
    pub presets_dialog: Option<super::presets_dialog::PresetsDialog>,
    pub show_agent_config: bool,
    pub agent_config_dialog: Option<super::agent_config::AgentConfigDialog>,
    pub agent_chain_state: AgentChainState,
    pub agent_chain_expanded: Vec<usize>,
    /// Channel sender for relaying core events (EngineEvent, AppEvent) to the UI thread.
    /// The corresponding receiver is stored separately so `process_pending_events` can poll it.
    pub pending_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub pending_rx: Option<mpsc::Receiver<AppEvent>>,
    pub agent_cancel_token: CancellationToken,
    /// Persistent chat engine — created once in `new()` and reused across sends.
    pub chat_engine: Option<crate::client::engine::ChatEngine>,
    /// Index of the currently selected agent for chat (None = auto-select).
    pub selected_agent_index: Option<usize>,
    /// Remote n_ctx value (for remote mode).
    pub remote_n_ctx: u32,
    /// Handle for the remote n_ctx update task.
    pub remote_n_ctx_handle: Option<JoinHandle<()>>,
    /// Arc for the remote n_ctx atomic value.
    pub remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,
}

impl ChatApp {
    pub fn new(
        config: Config,
        client: ChatClient,
        server: ServerManager,
        tool_manager: Arc<ToolManager>,
        agent_engine: Arc<crate::agents::AgentEngine>,
    ) -> Self {
        // Create the event channel pair: UI polls the receiver each frame,
        // the merge task (started in send_message) writes into the sender.
        let (tx, rx) = mpsc::channel::<AppEvent>();
        // Initialize the sessions panel with a clone of the config.
        let sessions_panel =
            super::sessions_panel::SessionsPanel::new(&Arc::new(Mutex::new(config.clone())));
        Self {
            config,
            client,
            server,
            tool_manager,
            agent_engine,
            cancellation_token: CancellationToken::new(),
            chat: ChatAreaState::new(),
            sessions: SessionsPanelState {
                sessions: Vec::new(),
                selected_session: None,
                sessions_panel: Some(sessions_panel),
            },
            status: AppStatus::Stopped,
            show_settings: false,
            settings_dialog: None,
            presets_dialog: None,
            show_agent_config: false,
            agent_config_dialog: None,
            agent_chain_state: AgentChainState::default(),
            agent_chain_expanded: Vec::new(),
            pending_tx: Some(Arc::new(Mutex::new(tx))),
            pending_rx: Some(rx),
            agent_cancel_token: CancellationToken::new(),
            chat_engine: None,
            selected_agent_index: None,
            remote_n_ctx: 0,
            remote_n_ctx_handle: None,
            remote_n_ctx_arc: None,
        }
    }

    /// Centralized config save — all callers should use this.
    pub fn save_config(&mut self) -> Result<(), crate::config::Error> {
        self.config.streaming = self.chat.streaming;
        self.config.chat_history = self.chat.messages.iter().map(|m| crate::config::ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: if m.timestamp.is_empty() {
                crate::types::format_timestamp()
            } else {
                m.timestamp.clone()
            },
        }).collect();
        self.config.save()
    }

    /// Centralized session save — delegates to client.
    pub fn save_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.client.clone().save_session().map_err(|e| e.into())
    }

}

/// State for the chat area.
pub struct ChatAreaState {
    pub messages: Vec<ChatMessage>,
    pub input_text: String,
    pub is_streaming: bool,
    pub stream_buffer: String,
    pub is_pipeline_running: bool,
    pub pending_error: Option<String>,
    pub is_generating: bool,
    pub streaming: bool,
    pub prev_scroll_offset_y: f32,
    pub prev_content_height: f32,
    pub scroll_to_bottom_requested: bool,
    pub current_thinking: String,
    pub current_response: String,
    pub at_bottom: bool,
    pub scroll_offset_y: f32,
    pub button_opacity: f32,
    pub button_visible: bool,
    pub editing_message_index: Option<usize>,
    pub editing_message_content: String,
    pub context_used: f32,
    pub pipeline: Option<PipelineState>,
    pub pending_image: Option<egui::ImageSource<'static>>,
    pub status: crate::types::AppStatus,
    pub token_count: usize,
    pub engine: Option<Arc<crate::agents::AgentEngine>>,
    pub streaming_task: Option<tokio::task::JoinHandle<()>>,
}

// Implement Clone manually for ChatAreaState since JoinHandle doesn't implement Clone
impl Clone for ChatAreaState {
    fn clone(&self) -> Self {
        Self {
            messages: self.messages.clone(),
            input_text: self.input_text.clone(),
            is_streaming: self.is_streaming,
            stream_buffer: self.stream_buffer.clone(),
            is_pipeline_running: self.is_pipeline_running,
            pending_error: self.pending_error.clone(),
            is_generating: self.is_generating,
            streaming: self.streaming,
            prev_scroll_offset_y: self.prev_scroll_offset_y,
            prev_content_height: self.prev_content_height,
            scroll_to_bottom_requested: self.scroll_to_bottom_requested,
            current_thinking: self.current_thinking.clone(),
            current_response: self.current_response.clone(),
            at_bottom: self.at_bottom,
            scroll_offset_y: self.scroll_offset_y,
            button_opacity: self.button_opacity,
            button_visible: self.button_visible,
            editing_message_index: self.editing_message_index,
            editing_message_content: self.editing_message_content.clone(),
            context_used: self.context_used,
            pipeline: self.pipeline.clone(),
            pending_image: self.pending_image.clone(),
            status: self.status.clone(),
            token_count: self.token_count,
            engine: self.engine.clone(),
            streaming_task: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PipelineState {
    pub plan_id: String,
    pub iteration: usize,
    pub cancelled: bool,
    pub tasks: Vec<PipelineTaskEntry>,
    pub feedback_state: Option<String>,
}

impl ChatAreaState {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            input_text: String::new(),
            is_streaming: false,
            stream_buffer: String::new(),
            is_pipeline_running: false,
            pending_error: None,
            is_generating: false,
            streaming: false,
            prev_scroll_offset_y: 0.0,
            prev_content_height: 0.0,
            scroll_to_bottom_requested: false,
            current_thinking: String::new(),
            current_response: String::new(),
            at_bottom: true,
            scroll_offset_y: 0.0,
            button_opacity: 1.0,
            button_visible: true,
            editing_message_index: None,
            editing_message_content: String::new(),
            context_used: 0.0,
            pipeline: None,
            pending_image: None,
            status: crate::types::AppStatus::Stopped,
            token_count: 0,
            engine: None,
            streaming_task: None,
        }
    }

    pub fn append_message(&mut self, role: &str, content: &str) {
        self.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: crate::types::format_timestamp(),
            image: None,
        });
    }

    pub fn stream_chunk(&mut self, chunk: &str) {
        self.stream_buffer.push_str(chunk);
    }

    pub fn commit_stream(&mut self) {
        let buffer = self.stream_buffer.clone();
        if !buffer.is_empty() {
            self.append_message("assistant", &buffer);
            self.stream_buffer.clear();
        }
        self.is_streaming = false;
    }
}

impl Default for ChatAreaState {
    fn default() -> Self {
        Self::new()
    }
}

/// State for the sessions panel.
#[derive(Clone, Debug)]
pub struct SessionsPanelState {
    pub sessions: Vec<crate::sessions::model::Session>,
    pub selected_session: Option<String>,
    pub sessions_panel: Option<super::sessions_panel::SessionsPanel>,
}

impl SessionsPanelState {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected_session: None,
            sessions_panel: None,
        }
    }
}

impl Default for SessionsPanelState {
    fn default() -> Self {
        Self::new()
    }
}
