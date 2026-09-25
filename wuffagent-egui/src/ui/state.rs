use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use wuffagent_core::config::Config;
use wuffagent_core::server::ServerManager;
use wuffagent_core::tools::ToolManager;

use wuffagent_core::types::{AppEvent, AppStatus};

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
/// Fields are grouped into small sub-structs by responsibility (see
/// [`groups`]) and embedded as `pub` fields, so UI modules access them via
/// `self.<group>.<field>`:
///
/// - **`core`** — app-wide service handles created at startup.
/// - **`sessions`** — per-session runtime state + selection.
/// - **`dialogs`** — transient dialog/panel widgets + visibility flags.
/// - **`relay`** — core → UI event channel.
/// - **`remote`** — server-synced context window state (n_ctx).
/// - **`display`** — cached rendered messages (perf) + status bar.
/// - **`restart`** — restart / auto-resume lifecycle.
pub mod groups;
pub struct ChatApp {
    /// App-wide service handles (server, connection, engine, memory, MCP, tools).
    pub core: groups::CoreServices,
    /// Per-session runtime state + session selection + chat-input staging.
    pub sessions: groups::SessionState,
    /// The sessions sidebar widget (manages its own list + selection).
    pub sessions_panel: Option<super::sessions_panel::SessionsPanel>,
    /// Transient dialog/panel widgets and their visibility flags.
    pub dialogs: groups::Dialogs,
    /// Core → UI event channel.
    pub relay: groups::EventRelay,
    /// Server-synced context window state (remote n_ctx).
    pub remote: groups::RemoteNctx,
    /// Cached rendered messages (perf) + status bar.
    pub display: groups::DisplayState,
    /// Restart / auto-resume lifecycle state.
    pub restart: groups::RestartState,
}

impl ChatApp {
    pub fn new(
        config: Config,
        server: ServerManager,
        tool_manager: Arc<ToolManager>,
        agent_engine: Arc<wuffagent_core::agents::AgentEngine>,
        connection: Arc<wuffagent_core::client::ConnectionSettings>,
        session_store: HashMap<String, wuffagent_core::sessions::SessionRuntime>,
        selected_session_id: Option<String>,
        event_tx: mpsc::Sender<AppEvent>,
        event_rx: mpsc::Receiver<AppEvent>,
        memory_manager: Arc<wuffagent_core::memory::MemoryManager>,
        memory_runtime: Arc<tokio::runtime::Runtime>,
        mcp_manager: Arc<wuffagent_core::tools::mcp::McpManager>,
        auto_resume_reason: Option<String>,
        auto_resume_failed: Option<(String, String)>,
    ) -> Self {
        // Build the sessions sidebar widget, pre-selecting the active session.
        let mut panel = super::sessions_panel::SessionsPanel::new(&Arc::new(Mutex::new(config.clone())));
        if let Some(id) = &selected_session_id {
            panel.select_session(id);
        }
        let sessions_panel = Some(panel);
        Self {
            core: groups::CoreServices {
                config,
                server,
                connection,
                last_synced_base_url: String::new(),
                tool_manager,
                agent_engine,
                memory_manager,
                memory_runtime: RuntimeOnThread::new(memory_runtime),
                mcp_manager,
            },
            sessions: groups::SessionState {
                session_store,
                selected_session_id,
                sub_session_tabs: Vec::new(),
                active_tab: None,
                pending_images: HashMap::new(),
            },
            sessions_panel,
            dialogs: groups::Dialogs {
                show_settings: false,
                settings_dialog: None,
                presets_dialog: None,
                show_agent_config: false,
                agent_config_dialog: None,
                memory_panel: super::memory_panel::MemoryPanel::new(),
                usage_panel: super::usage_panel::UsagePanel::new(),
                mcp_panel: super::mcp_panel::McpPanel::new(),
                improvements_panel: super::improvements::ImprovementsPanel::new(),
            },
            relay: groups::EventRelay {
                pending_tx: Some(Arc::new(Mutex::new(event_tx))),
                pending_rx: Some(event_rx),
            },
            remote: groups::RemoteNctx {
                remote_n_ctx: 0,
                remote_n_ctx_handle: None,
                remote_n_ctx_arc: None,
            },
            display: groups::DisplayState {
                display_snapshot: std::sync::Arc::new(Vec::new()),
                snapshot_session: None,
                snapshot_len: 0,
                display_dirty: true,
                status: AppStatus::Stopped,
            },
            restart: groups::RestartState {
                pending_restart: false,
                // A marker whose session failed to load can never resume.
                pending_auto_resume: auto_resume_reason.is_some() && auto_resume_failed.is_none(),
                auto_resume_reason,
                resume_failed: auto_resume_failed,
            },
        }
    }

    /// Centralized config save — all callers should use this.
    ///
    /// Note: the reasoning-effort selection is per-session (stored in the
    /// session file via `SessionRuntime.reasoning_mode`), not part of the
    /// global config — `config.reasoning_effort` only seeds non-session
    /// clients (bootstrap engine, non-streaming adapter, memory LLM).
    pub fn save_config(&mut self) -> Result<(), wuffagent_core::config::Error> {
        self.core.config.save()
    }

    /// Sync the selected session's per-session UI selections (chosen agent
    /// profile + reasoning-effort mode) into its client so the next
    /// `save_session` persists them with the session file. Also keeps the
    /// client's forced wire level in sync for non-pipeline requests (the
    /// chat pipeline re-applies the mode on every run anyway).
    pub fn sync_session_meta(&mut self, id: &str) {
        if let Some(runtime) = self.sessions.session_store.get_mut(id) {
            // Update only the UI selections; clone the existing meta first so
            // non-UI fields (the sub-session parent link) survive the sync.
            let mut meta = runtime.client.session_meta().clone();
            meta.selected_agent = runtime.selected_agent.clone();
            meta.reasoning_mode = runtime.reasoning_mode;
            runtime.client.set_session_meta(meta);
            runtime.client.set_reasoning_effort(match runtime.reasoning_mode {
                wuffagent_core::types::ReasoningMode::Auto => {
                    wuffagent_core::types::ReasoningEffort::Off
                }
                wuffagent_core::types::ReasoningMode::Explicit(e) => e,
            });
        }
    }

    /// Save the selected session's conversation.
    pub fn save_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Clone the id so the immutable borrow of `self.sessions.selected_session_id`
        // ends before we call `save_session_for` (which mutably borrows `self`).
        let id = self.sessions.selected_session_id.clone();
        match id {
            Some(id) => self.save_session_for(&id),
            None => Ok(()),
        }
    }

    /// Save a specific session's conversation by id.
    pub fn save_session_for(&mut self, id: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(runtime) = self.sessions.session_store.get(id) {
            runtime.client.save_session().map_err(|e| e.into())
        } else {
            Ok(())
        }
    }

    /// Apply a pending sessions-panel action (create/delete/rename/export/import).
    ///
    /// Thin shim delegating to `sessions_actions::apply_sessions_action` (U2);
    /// the free fn there holds `&mut ChatApp` so this method keeps the single
    /// mutable borrow of `self.sessions_panel`.
    pub fn apply_sessions_action(
        &mut self,
        action: super::sessions_actions::PanelAction,
    ) {
        super::sessions_actions::apply_sessions_action(self, action);
    }

    /// The session currently displayed in the chat area: the active sub-
    /// session tab if one is open, otherwise the selected (main) session.
    pub fn displayed_session_id(&self) -> Option<&str> {
        self.sessions.active_tab
            .as_deref()
            .or(self.sessions.selected_session_id.as_deref())
    }

    /// Owned form of [`Self::displayed_session_id`] — the id to target with
    /// input-area operations (send/stop/attach, agent/reasoning selection,
    /// input text): the session the user is currently looking at.
    pub fn input_target_session_id(&self) -> Option<String> {
        self.displayed_session_id().map(|s| s.to_string())
    }

    /// Get the selected session's chat area state (immutable view).
    pub fn selected_chat_state(&self) -> Option<&wuffagent_core::sessions::ChatAreaState> {
        self.sessions.selected_session_id
            .as_ref()
            .and_then(|id| self.sessions.session_store.get(id))
            .map(|r| &r.chat_state)
    }

    /// Get the client for the selected session (if any).
    pub fn active_client(&self) -> Option<&wuffagent_core::client::ChatClient> {
        self.sessions.selected_session_id
            .as_ref()
            .and_then(|id| self.sessions.session_store.get(id))
            .map(|r| &r.client)
    }

    /// Relaunch WuffAgent: persist the restart marker (so the new process resumes
    /// the current session) and spawn the (optionally newly built) executable with
    /// the current CLI args. Sets `pending_restart` so the window closes next frame.
    pub fn perform_restart(&mut self, reason: String, exe_path: Option<String>) {
        let session_id = self.sessions.selected_session_id.clone().unwrap_or_default();
        let marker_path = wuffagent_core::config::get_restart_marker_path();
        let marker = wuffagent_core::config::RestartMarker { session_id, reason };
        match serde_json::to_string_pretty(&marker) {
            Ok(json) => {
                if let Some(parent) = marker_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&marker_path, json) {
                    tracing::warn!(path = %marker_path.display(), error = %e, "Failed to write restart marker");
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to serialize restart marker"),
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
        // T4: when switching to a DIFFERENT binary, back up the currently
        // running exe to `<exe>.prev` — a rollback point for the case where
        // the new binary crashes on startup. Best-effort: a copy failure
        // (e.g. permissions) is logged, never fatal.
        if let Ok(current) = std::env::current_exe() {
            let same = current
                .to_string_lossy()
                .eq_ignore_ascii_case(&exe);
            if !same {
                let prev = current.with_file_name(format!(
                    "{}.prev",
                    current
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ));
                match std::fs::copy(&current, &prev) {
                    Ok(_) => tracing::info!(backup = %prev.display(), "Backed up running exe before restart"),
                    Err(e) => tracing::warn!(backup = %prev.display(), error = %e, "Could not back up running exe before restart (continuing)"),
                }
            }
        }
        let args: Vec<String> = std::env::args().skip(1).collect();
        match std::process::Command::new(&exe).args(&args).spawn() {
            Ok(_) => self.restart.pending_restart = true,
            Err(e) => tracing::error!(exe = %exe, error = %e, "Failed to relaunch WuffAgent"),
        }
    }
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        // Disconnect all MCP servers (kills child processes) and drop the
        // MCP runtime on a plain thread — the UI thread is inside the main
        // runtime's context, where dropping a runtime panics. Non-blocking.
        self.core.mcp_manager.shutdown();
    }
}
