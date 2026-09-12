use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::server::ServerManager;
use crate::tools::ToolManager;

use crate::types::{AppEvent, AppStatus};

/// A message sent while the AI is still working (now stored per-session in core).
pub use crate::sessions::QueuedMessage;

/// Main application state for the egui UI.
pub struct ChatApp {
    pub config: Config,
    pub server: ServerManager,
    pub tool_manager: Arc<ToolManager>,
    pub agent_engine: Arc<crate::agents::AgentEngine>,
    /// Shared memory manager (single-writer discipline; all UI memory writes go
    /// through it). Shared with the agent engine and the memory tools.
    pub memory_manager: Arc<crate::memory::MemoryManager>,
    /// Dedicated runtime for UI-triggered async memory work (maintenance pass).
    pub memory_runtime: tokio::runtime::Runtime,
    /// Clone of the bootstrap non-streaming LLM client (shares interior-mutable
    /// `base_url` with the adapter, so a URL change here propagates in-place).
    pub bootstrap_llm_client: crate::client::ChatClient,
    /// Clone of the bootstrap streaming client (also held by the engine).
    pub bootstrap_stream_client: crate::client::ChatClient,
    /// Last base_url synced to per-session clients (no-op guard).
    pub last_synced_base_url: String,
    /// Per-session runtime state keyed by session ID.
    pub session_store: HashMap<String, crate::sessions::SessionRuntime>,
    /// ID of the currently selected session (None = no session selected).
    pub selected_session_id: Option<String>,
    /// The sessions sidebar widget (manages its own list + selection).
    pub sessions_panel: Option<super::sessions_panel::SessionsPanel>,
    pub status: AppStatus,
    pub show_settings: bool,
    pub settings_dialog: Option<super::settings::SettingsDialog>,
    pub presets_dialog: Option<super::presets_dialog::PresetsDialog>,
    pub show_agent_config: bool,
    pub agent_config_dialog: Option<super::agent_config::AgentConfigDialog>,
    /// The memory panel widget (owns its own list/search/edit state).
    /// `show_panel` gates whether the window is drawn.
    pub memory_panel: super::memory_panel::MemoryPanel,
    /// Channel sender for relaying core events (EngineEvent, AppEvent) to the UI thread.
    /// The corresponding receiver is stored separately so `process_pending_events` can poll it.
    pub pending_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub pending_rx: Option<mpsc::Receiver<AppEvent>>,
    pub agent_cancel_token: CancellationToken,
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
    /// Shared display snapshot of the selected session's messages. Rebuilt only
    /// when the session or its message set changes, so a per-frame redraw is an
    /// O(1) `Arc::clone` instead of a full deep clone of every message (which
    /// copies large tool outputs + base64 images and is the main lag).
    pub display_snapshot: std::sync::Arc<Vec<crate::types::ChatMessage>>,
    /// Session id the snapshot belongs to (None = empty).
    pub snapshot_session: Option<String>,
    /// Message count the snapshot was built from.
    pub snapshot_len: usize,
    /// Set on in-place message edits (which keep the count unchanged) to force a rebuild.
    pub display_dirty: bool,
}

impl ChatApp {
    pub fn new(
        config: Config,
        server: ServerManager,
        tool_manager: Arc<ToolManager>,
        agent_engine: Arc<crate::agents::AgentEngine>,
        bootstrap_llm_client: crate::client::ChatClient,
        bootstrap_stream_client: crate::client::ChatClient,
        session_store: HashMap<String, crate::sessions::SessionRuntime>,
        selected_session_id: Option<String>,
        event_tx: mpsc::Sender<AppEvent>,
        event_rx: mpsc::Receiver<AppEvent>,
        memory_manager: Arc<crate::memory::MemoryManager>,
        memory_runtime: tokio::runtime::Runtime,
    ) -> Self {
        let reasoning_effort = config.reasoning_effort;
        // Build the sessions sidebar widget, pre-selecting the active session.
        let mut panel = super::sessions_panel::SessionsPanel::new(&Arc::new(Mutex::new(config.clone())));
        if let Some(id) = &selected_session_id {
            panel.select_session(id);
        }
        let sessions_panel = Some(panel);
        Self {
            config,
            server,
            tool_manager,
            agent_engine,
            memory_manager,
            memory_runtime,
            bootstrap_llm_client,
            bootstrap_stream_client,
            last_synced_base_url: String::new(),
            session_store,
            selected_session_id,
            sessions_panel,
            status: AppStatus::Stopped,
            show_settings: false,
            settings_dialog: None,
            presets_dialog: None,
            show_agent_config: false,
            agent_config_dialog: None,
            memory_panel: super::memory_panel::MemoryPanel::new(),
            pending_tx: Some(Arc::new(Mutex::new(event_tx))),
            pending_rx: Some(event_rx),
            agent_cancel_token: CancellationToken::new(),
            reasoning_effort,
            improvements_panel: super::improvements::ImprovementsPanel::new(),
            display_snapshot: std::sync::Arc::new(Vec::new()),
            snapshot_session: None,
            snapshot_len: 0,
            display_dirty: true,
            remote_n_ctx: 0,
            remote_n_ctx_handle: None,
            remote_n_ctx_arc: None,
        }
    }

    /// Centralized config save — all callers should use this.
    pub fn save_config(&mut self) -> Result<(), crate::config::Error> {
        self.config.reasoning_effort = self.reasoning_effort;
        self.config.save()
    }

    /// Save the selected session's conversation.
    pub fn save_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Clone the id so the immutable borrow of `self.selected_session_id`
        // ends before we call `save_session_for` (which mutably borrows `self`).
        let id = self.selected_session_id.clone();
        match id {
            Some(id) => self.save_session_for(&id),
            None => Ok(()),
        }
    }

    /// Save a specific session's conversation by id.
    pub fn save_session_for(&mut self, id: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(runtime) = self.session_store.get(id) {
            runtime.client.save_session().map_err(|e| e.into())
        } else {
            Ok(())
        }
    }

    /// Apply a pending sessions-panel action (create/delete/rename/export/import).
    ///
    /// This inlines the per-action logic (previously in `apply_actions`) so we
    /// only ever hold a single mutable borrow of `self.sessions_panel` at a
    /// time — calling a free function with both `&mut self` and a `&mut` field
    /// of `self` would be a conflicting double-borrow.
    pub fn apply_sessions_action(
        &mut self,
        action: super::sessions_actions::PanelAction,
    ) {
        use super::sessions_actions::PanelAction;
        use std::path::PathBuf;

        let Some(panel) = self.sessions_panel.as_mut() else {
            return;
        };
        let sessions_dir = panel.sessions_dir().clone();

        match action {
            PanelAction::Rename { id, new_name } => {
                if let Some(mut s) = crate::sessions::load_session(&sessions_dir, &id) {
                    s.name = new_name.clone();
                    let _ = crate::sessions::save_session(&sessions_dir, &s);
                }
                if let Some(runtime) = self.session_store.get_mut(&id) {
                    runtime.name = new_name;
                }
                panel.refresh();
            }
            PanelAction::Create(name) => {
                let session = crate::sessions::create_session(&sessions_dir, &name);

                let base_url = self.config.base_url();
                let mut session_client = crate::client::ChatClient::new(&base_url);
                session_client.set_api_key(self.config.remote_api_key.as_deref());
                session_client.set_session(Some(session.id.clone()), sessions_dir.clone());
                session_client.set_reasoning_effort(self.config.reasoning_effort);
                session_client.set_max_messages(self.config.max_messages);
                session_client.set_n_ctx(self.config.n_ctx);
                if self.config.encryption_enabled {
                    if let Some(key) = self.config.encryption_key() {
                        session_client.set_encryption_key(Some(key));
                    }
                }
                let _ = session_client.load_session();

                session_client.set_tool_event_sender(
                    self.pending_tx.as_ref().unwrap().lock().unwrap().clone(),
                );

                // Per-session engine: derived from the shared bootstrap engine
                // but bound to THIS session's client so the agent chat loop
                // reads/writes an isolated conversation store (the shared
                // bootstrap engine's client would otherwise be mutated by every
                // session in parallel — a cross-session data race).
                let session_engine = (*self.agent_engine)
                    .clone()
                    .with_client(session_client.clone())
                    .with_session_id(session.id.clone());
                let pipeline = crate::client::ChatPipeline::new(
                    Arc::new(session_engine.clone()),
                    self.pending_tx.as_ref().unwrap().lock().unwrap().clone(),
                    self.config.reasoning_effort,
                    session.id.clone(),
                );
                let cancel_token = tokio_util::sync::CancellationToken::new();

                let mut runtime = crate::sessions::SessionRuntime::new(
                    session.id.clone(),
                    session.name.clone(),
                    session_client,
                    pipeline,
                    session_engine,
                    cancel_token,
                );

                // Default the new session agent to "general" (per-session
                // selection shown in the input selector).
                runtime.selected_agent = Some("general".to_string());

                self.session_store.insert(session.id.clone(), runtime);
                *panel.selected_id_mut() = Some(session.id.clone());

                {
                    let mut cfg = panel.config().clone();
                    cfg.session_id = Some(session.id.clone());
                    if let Err(e) = cfg.save() {
                        eprintln!("Failed to save config after creating session: {}", e);
                    }
                }
                panel.refresh();
            }
            PanelAction::Delete(id) => {
                match crate::sessions::delete_session(&sessions_dir, &id) {
                    Ok(()) => {
                        self.session_store.remove(&id);
                        if self.selected_session_id.as_deref() == Some(&*id) {
                            self.selected_session_id = None;
                        }
                        panel.show_notification(&format!("Session '{}' deleted", id), true);
                        panel.clear_session();
                        panel.refresh();
                        let mut cfg = panel.config().clone();
                        cfg.session_id = None;
                        if let Err(e) = cfg.save() {
                            eprintln!("Failed to save config after deleting session: {}", e);
                        }
                        panel.show_notification("Session deleted", true);
                    }
                    Err(e) => {
                        panel.show_notification(&format!("Failed to delete session: {}", e), false);
                        if panel.selected_id().as_deref() == Some(&*id) {
                            *panel.selected_id_mut() = None;
                        }
                        panel.refresh();
                    }
                }
            }
            PanelAction::Export { session_id } => {
                let output_path = if panel.export_path().is_empty() {
                    PathBuf::from(format!("{}.json", session_id))
                } else {
                    PathBuf::from(panel.export_path())
                };
                match crate::sessions::export_session(&sessions_dir, &session_id, &output_path) {
                    Ok(()) => panel.show_notification(&format!("Exported to {}", output_path.display()), true),
                    Err(e) => panel.show_notification(&format!("Export failed: {}", e), false),
                }
            }
            PanelAction::Import => {
                let input_path = if panel.import_path().is_empty() {
                    PathBuf::from("session.json")
                } else {
                    PathBuf::from(panel.import_path())
                };
                match crate::sessions::import_session(&sessions_dir, &input_path) {
                    Ok(new_id) => {
                        panel.show_notification(&format!("Imported session: {}", new_id), true);
                        panel.refresh();
                    }
                    Err(e) => {
                        panel.show_notification(&format!("Import failed: {}", e), false);
                    }
                }
            }
        }
    }

    /// Get the selected session's chat area state (immutable view).
    pub fn selected_chat_state(&self) -> Option<&crate::sessions::ChatAreaState> {
        self.selected_session_id
            .as_ref()
            .and_then(|id| self.session_store.get(id))
            .map(|r| &r.chat_state)
    }

    /// Get the client for the selected session (if any).
    pub fn active_client(&self) -> Option<&crate::client::ChatClient> {
        self.selected_session_id
            .as_ref()
            .and_then(|id| self.session_store.get(id))
            .map(|r| &r.client)
    }
}
