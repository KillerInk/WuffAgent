use std::sync::{Arc, Mutex};

use crate::config::PresetStore;
use crate::config::Config;
use crate::sessions::model::AgentChainEntry;
use crate::types::{AppStatus, ChatMessage};
use super::backend::Backend;

/// Scroll tracking state.
#[derive(Clone, Debug)]
pub struct ScrollState {
    pub at_bottom: bool,
    pub follow_bottom: bool,
    pub jump_button_opacity: f32,
}

impl Default for ScrollState {
    fn default() -> Self {
        ScrollState {
            at_bottom: true,
            follow_bottom: true,
            jump_button_opacity: 0.0,
        }
    }
}

/// Chat-related state.
#[derive(Default)]
pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub input_text: String,
    pub is_generating: bool,
    pub status: AppStatus,
    pub streaming: bool,
    pub current_response: String,
    pub current_thinking: String,
    pub token_count: u32,
    pub context_used: f32,
    pub pending_image: Option<String>,
    pub editing_message_index: Option<usize>,
    pub editing_message_content: String,
}

/// Session list state.
#[derive(Default)]
pub struct SessionsState {
    pub sessions: Vec<crate::sessions::model::Session>,
    pub selected_id: Option<String>,
    pub rename_target: Option<String>,
}

/// Agent pipeline + chain panel state.
#[derive(Default)]
pub struct PanelsState {
    pub pipeline_active: bool,
    pub pipeline_tasks: Vec<PipelineTaskEntry>,
    pub chain_active: bool,
    pub chain_entries: Vec<AgentChainEntry>,
}

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

/// Dialog type enum.
pub enum Dialog {
    Settings {
        config: Config,
        preset_store: PresetStore,
    },
    Presets {
        store: PresetStore,
    },
    AgentConfig,
}

/// Main application state for the iced UI.
pub struct AppState {
    pub chat: ChatState,
    pub sessions: SessionsState,
    pub panels: PanelsState,
    pub dialog: Option<Dialog>,
    pub theme_name: String,
    pub scroll: ScrollState,
    pub remote_n_ctx: u32,
    pub config: Arc<Mutex<Config>>,
    pub presets: PresetStore,
    pub backend: Arc<Backend>,
    pub error_toast: Option<String>,
}

impl AppState {
    pub fn new(
        config: Arc<Mutex<Config>>,
        presets: PresetStore,
        backend: Arc<Backend>,
    ) -> Self {
        let cfg = config.lock().unwrap();
        let theme_name = cfg.theme.clone();
        let n_ctx = cfg.n_ctx;
        drop(cfg);

        AppState {
            chat: ChatState::default(),
            sessions: SessionsState::default(),
            panels: PanelsState::default(),
            dialog: None,
            theme_name,
            scroll: ScrollState::default(),
            remote_n_ctx: n_ctx,
            config,
            presets,
            backend,
            error_toast: None,
        }
    }
}
