use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::server::ServerManager;
use crate::tools::ToolManager;

use crate::types::{AppEvent, AppStatus};

/// A message sent while the AI is still working (now stored per-session in core).
pub use crate::sessions::QueuedMessage;

/// A tokio `Runtime` (held in an `Arc`) that is dropped on a dedicated plain
/// OS thread.
///
/// Dropping a tokio `Runtime` performs a blocking wait for its worker threads.
/// If that drop happens on a thread that has already entered a tokio runtime
/// context (e.g. the main thread driving `#[tokio::main]`), tokio panics with
/// "Cannot drop a runtime in a context where blocking is not allowed".
///
/// `ChatApp` (which owns the memory runtime) is dropped by eframe on the main
/// thread after the window closes, and the main thread is still inside the
/// `#[tokio::main]` runtime context at that point. We therefore move the
/// `Arc` to a fresh thread with no runtime context and drop it there, so the
/// blocking shutdown wait (which only runs when the LAST `Arc` is dropped) is
/// always safe. Helper threads holding other `Arc` clones (the memory
/// maintenance thread) keep the runtime alive until they finish; those are
/// plain threads with no runtime context, so they can drop the last `Arc`
/// safely too.
pub struct RuntimeOnThread(Option<Arc<tokio::runtime::Runtime>>);

impl RuntimeOnThread {
    pub fn new(rt: Arc<tokio::runtime::Runtime>) -> Self {
        Self(Some(rt))
    }

    /// Borrow the inner runtime. Clone the returned `Arc` to keep the runtime
    /// alive on another thread (the maintenance helper thread does this so it
    /// can `block_on` the runtime safely).
    pub fn as_ref(&self) -> &Arc<tokio::runtime::Runtime> {
        self.0.as_ref().expect("memory runtime already dropped")
    }
}

impl Drop for RuntimeOnThread {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            // Drop the Arc on a plain thread (no tokio context entered), then
            // wait for it so shutdown is deterministic. If a maintenance
            // helper thread still holds a clone, the actual `Runtime` drop
            // (and its blocking shutdown wait) happens on that thread instead
            // — which is also outside any runtime context.
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                drop(rt);
                let _ = tx.send(());
            });
            let _ = rx.recv();
        }
    }
}

/// Main application state for the egui UI.
///
/// Fields are flat `pub` (UI modules access them directly via `self.<field>`)
/// but grouped by concern:
///
/// - **Core services** — app-wide handles created at startup.
/// - **Sessions** — per-session runtime state + selection.
/// - **Dialogs & panels** — transient UI windows.
/// - **Event relay** — core → UI event channel.
/// - **Remote n_ctx** — server-synced context window state.
/// - **Display snapshot** — cached rendered messages (perf).
pub struct ChatApp {
    // ── Core services (app-wide, created at startup) ─────────────────
    pub config: Config,
    pub server: ServerManager,
    /// Shared connection settings (URL + API key) used by every client in the
    /// app (session runtimes, bootstrap engine, non-streaming LLM adapter).
    /// One `update()` here propagates to all of them.
    pub connection: Arc<crate::client::ConnectionSettings>,
    /// Last base_url synced to the shared connection settings (no-op guard).
    pub last_synced_base_url: String,
    pub tool_manager: Arc<ToolManager>,
    pub agent_engine: Arc<crate::agents::AgentEngine>,
    /// Shared memory manager (single-writer discipline; all UI memory writes go
    /// through it). Shared with the agent engine and the memory tools.
    pub memory_manager: Arc<crate::memory::MemoryManager>,
    /// Dedicated runtime for UI-triggered async memory work (maintenance pass).
    /// Wrapped in [`RuntimeOnThread`] so its `Drop` (a blocking wait) never
    /// runs on a thread inside another runtime's async context.
    pub memory_runtime: RuntimeOnThread,
    /// MCP (Model Context Protocol) manager: owns the dedicated runtime for
    /// MCP I/O and mirrors connected servers' tools into the shared registry.
    /// Its own `shutdown()` (called in `Drop`) disconnects all servers and
    /// drops that runtime on a plain thread.
    pub mcp_manager: Arc<crate::tools::mcp::McpManager>,

    // ── Sessions ─────────────────────────────────────────────────────
    /// Per-session runtime state keyed by session ID.
    pub session_store: HashMap<String, crate::sessions::SessionRuntime>,
    /// ID of the currently selected session (None = no session selected).
    pub selected_session_id: Option<String>,
    /// The sessions sidebar widget (manages its own list + selection).
    pub sessions_panel: Option<super::sessions_panel::SessionsPanel>,
    /// Reasoning effort for reasoning models (Off = omitted from requests).
    pub reasoning_effort: crate::types::ReasoningEffort,

    // ── Dialogs & panels (transient UI windows) ──────────────────────
    pub status: AppStatus,
    pub show_settings: bool,
    pub settings_dialog: Option<super::settings::SettingsDialog>,
    pub presets_dialog: Option<super::presets_dialog::PresetsDialog>,
    pub show_agent_config: bool,
    pub agent_config_dialog: Option<super::agent_config::AgentConfigDialog>,
    /// The memory panel widget (owns its own list/search/edit state).
    /// `show_panel` gates whether the window is drawn.
    pub memory_panel: super::memory_panel::MemoryPanel,
    /// The MCP panel widget (server list, add/edit, per-tool toggles).
    pub mcp_panel: super::mcp_panel::McpPanel,
    /// Pending agent improvement suggestions.
    pub improvements_panel: super::improvements::ImprovementsPanel,

    // ── Event relay (core → UI) ──────────────────────────────────────
    /// Channel sender for relaying core events (EngineEvent, AppEvent) to the UI thread.
    /// The corresponding receiver is stored separately so `process_pending_events` can poll it.
    pub pending_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub pending_rx: Option<mpsc::Receiver<AppEvent>>,

    // ── Remote n_ctx (server-synced context window) ──────────────────
    /// Remote n_ctx value (for remote mode).
    pub remote_n_ctx: u32,
    /// Handle for the remote n_ctx update task.
    pub remote_n_ctx_handle: Option<JoinHandle<()>>,
    /// Arc for the remote n_ctx atomic value.
    pub remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,

    // ── Display snapshot (perf: avoid deep-cloning messages per frame) ─
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

    // ── Restart / auto-resume ─────────────────────────────────────────
    /// Set when a `restart` tool run requested a relaunch (marker written + new
    /// process spawned); the window closes on the next frame.
    pub pending_restart: bool,
    /// Set once by `main` when a restart marker was found at startup; the first
    /// frame where the resumed session's runtime exists auto-sends the resume turn.
    pub pending_auto_resume: bool,
    /// Reason captured from the restart marker, used to build the resume turn.
    pub auto_resume_reason: Option<String>,
}

impl ChatApp {
    pub fn new(
        config: Config,
        server: ServerManager,
        tool_manager: Arc<ToolManager>,
        agent_engine: Arc<crate::agents::AgentEngine>,
        connection: Arc<crate::client::ConnectionSettings>,
        session_store: HashMap<String, crate::sessions::SessionRuntime>,
        selected_session_id: Option<String>,
        event_tx: mpsc::Sender<AppEvent>,
        event_rx: mpsc::Receiver<AppEvent>,
        memory_manager: Arc<crate::memory::MemoryManager>,
        memory_runtime: Arc<tokio::runtime::Runtime>,
        mcp_manager: Arc<crate::tools::mcp::McpManager>,
        auto_resume_reason: Option<String>,
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
            memory_runtime: RuntimeOnThread::new(memory_runtime),
            mcp_manager,
            connection,
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
            mcp_panel: super::mcp_panel::McpPanel::new(),
            pending_tx: Some(Arc::new(Mutex::new(event_tx))),
            pending_rx: Some(event_rx),
            reasoning_effort,
            improvements_panel: super::improvements::ImprovementsPanel::new(),
            display_snapshot: std::sync::Arc::new(Vec::new()),
            snapshot_session: None,
            snapshot_len: 0,
            display_dirty: true,
            remote_n_ctx: 0,
            remote_n_ctx_handle: None,
            remote_n_ctx_arc: None,
            pending_restart: false,
            pending_auto_resume: auto_resume_reason.is_some(),
            auto_resume_reason,
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

                let mut runtime = crate::sessions::SessionRuntime::create_from_config(
                    &self.config,
                    &self.connection,
                    &self.agent_engine,
                    session.id.clone(),
                    session.name.clone(),
                    self.pending_tx.as_ref().unwrap().lock().unwrap().clone(),
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

    /// Relaunch WuffAgent: persist the restart marker (so the new process resumes
    /// the current session) and spawn the (optionally newly built) executable with
    /// the current CLI args. Sets `pending_restart` so the window closes next frame.
    pub fn perform_restart(&mut self, reason: String, exe_path: Option<String>) {
        let session_id = self.selected_session_id.clone().unwrap_or_default();
        let marker_path = wuffagent_core::config::get_restart_marker_path();
        let marker = wuffagent_core::config::RestartMarker { session_id, reason };
        match serde_json::to_string_pretty(&marker) {
            Ok(json) => {
                if let Some(parent) = marker_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&marker_path, json) {
                    eprintln!("Failed to write restart marker: {}", e);
                }
            }
            Err(e) => eprintln!("Failed to serialize restart marker: {}", e),
        }
        // Spawn the new process with the current CLI args (minus the program name).
        // On Windows the running exe is locked, so a self-build should point
        // `exe_path` at the freshly built copy (see the restart tool's guidance).
        let exe = match exe_path {
            Some(p) if !p.is_empty() => p,
            _ => std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let args: Vec<String> = std::env::args().skip(1).collect();
        match std::process::Command::new(&exe).args(&args).spawn() {
            Ok(_) => self.pending_restart = true,
            Err(e) => eprintln!("Failed to relaunch WuffAgent ({:?}): {}", exe, e),
        }
    }
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        // Disconnect all MCP servers (kills child processes) and drop the
        // MCP runtime on a plain thread — the UI thread is inside the main
        // runtime's context, where dropping a runtime panics. Non-blocking.
        self.mcp_manager.shutdown();
    }
}
