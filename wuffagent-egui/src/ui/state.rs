use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::client::ChatClient;
use crate::config::Config;
use crate::server::ServerManager;
use crate::tools::ToolManager;

use crate::types::{AppEvent, AppStatus, ChatMessage, MessageKind};

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

/// A message sent while the AI is still working. Displayed in the chat
/// immediately and processed as the next turn once the current run (and any
/// earlier queued messages) finishes.
#[derive(Clone)]
pub struct QueuedMessage {
    pub text: String,
    pub image: Option<egui::ImageSource<'static>>,
    /// System prompt resolved from the selected agent at send time.
    pub agent_prompt: String,
    /// Tool policy (allowed_tools + shell config) resolved from the selected agent.
    pub tool_policy: crate::client::pipeline::ChatToolPolicy,
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
    pub show_agent_chain: bool,
    /// Channel sender for relaying core events (EngineEvent, AppEvent) to the UI thread.
    /// The corresponding receiver is stored separately so `process_pending_events` can poll it.
    pub pending_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub pending_rx: Option<mpsc::Receiver<AppEvent>>,
    pub agent_cancel_token: CancellationToken,
    /// Persistent chat pipeline — created once in `new()` and reused across sends.
    pub chat_pipeline: Option<crate::client::ChatPipeline>,
    /// Messages sent while the AI was still working, processed in FIFO order
    /// once the current run finishes.
    pub queued_messages: Vec<QueuedMessage>,
    /// Index of the currently selected agent for chat (None = auto-select).
    pub selected_agent_index: Option<usize>,
    /// Reasoning effort for reasoning models (Off = omitted from requests).
    pub reasoning_effort: crate::types::ReasoningEffort,
    /// Remote n_ctx value (for remote mode).
    pub remote_n_ctx: u32,
    /// Handle for the remote n_ctx update task.
    pub remote_n_ctx_handle: Option<JoinHandle<()>>,
    /// Arc for the remote n_ctx atomic value.
    pub remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,
    /// Pending agent improvement suggestions.
    pub improvements_panel: super::improvements::ImprovementsPanel,
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
        let reasoning_effort = config.reasoning_effort;
        Self {
            config,
            client,
            server,
            tool_manager,
            agent_engine,
            cancellation_token: CancellationToken::new(),
            chat: ChatAreaState::new(),
            reasoning_effort,
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
            show_agent_chain: false,
            pending_tx: Some(Arc::new(Mutex::new(tx))),
            pending_rx: Some(rx),
            agent_cancel_token: CancellationToken::new(),
            chat_pipeline: None,
            queued_messages: Vec::new(),
            selected_agent_index: None,
            improvements_panel: super::improvements::ImprovementsPanel::new(),
            remote_n_ctx: 0,
            remote_n_ctx_handle: None,
            remote_n_ctx_arc: None,
        }
    }

    /// Centralized config save — all callers should use this.
    pub fn save_config(&mut self) -> Result<(), crate::config::Error> {
        self.config.reasoning_effort = self.reasoning_effort;
        self.client.set_reasoning_effort(self.reasoning_effort);
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
#[derive(Clone)]
pub struct ChatAreaState {
    pub messages: Vec<ChatMessage>,
    pub input_text: String,
    pub stream_buffer: String,
    pub pending_error: Option<String>,
    pub is_generating: bool,
    pub scroll_to_bottom_requested: bool,
    pub current_thinking: String,
    pub at_bottom: bool,
    pub button_opacity: f32,
    pub button_visible: bool,
    pub editing_message_index: Option<usize>,
    pub editing_message_content: String,
    pub expanded_messages: Vec<usize>,
    pub context_used: f32,
    pub pipeline: Option<PipelineState>,
    pub pending_image: Option<egui::ImageSource<'static>>,
    pub status: crate::types::AppStatus,
    pub token_count: usize,
    pub engine: Option<Arc<crate::agents::AgentEngine>>,
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
            stream_buffer: String::new(),
            pending_error: None,
            is_generating: false,
            scroll_to_bottom_requested: false,
            current_thinking: String::new(),
            at_bottom: true,
            button_opacity: 1.0,
            button_visible: true,
            editing_message_index: None,
            editing_message_content: String::new(),
            expanded_messages: Vec::new(),
            context_used: 0.0,
            pipeline: None,
            pending_image: None,
            status: crate::types::AppStatus::Stopped,
            token_count: 0,
            engine: None,
        }
    }

    pub fn push_message(&mut self, kind: MessageKind, role: &str, content: &str) {
        self.messages.push(ChatMessage {
            kind,
            role: role.to_string(),
            content: content.to_string(),
            timestamp: crate::types::format_timestamp(),
            image: None,
        });
    }

    pub fn append_message(&mut self, role: &str, content: &str) {
        self.push_message(MessageKind::Normal, role, content);
    }

    pub fn stream_chunk(&mut self, chunk: &str) {
        self.stream_buffer.push_str(chunk);
    }

    pub fn commit_stream(&mut self) {
        let buffer = std::mem::take(&mut self.stream_buffer);
        if !buffer.is_empty() {
            self.append_message("assistant", &buffer);
        }
    }

    /// Show a brief notification message (stored in pending_error for display).
    pub fn show_notification(&mut self, msg: &str, _success: bool) {
        // Store as a temporary notification message
        self.messages.push(ChatMessage {
            kind: MessageKind::Normal,
            role: "system".to_string(),
            content: format!("⚡ {}", msg),
            timestamp: crate::types::format_timestamp(),
            image: None,
        });
    }

    /// Reload the chat display from the client's current conversation.
    pub fn reload_messages_from_client(&mut self) {
        // This will be called after a session resume to refresh the display
        // The actual message reload happens via the session loading mechanism
        self.messages.clear();
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
