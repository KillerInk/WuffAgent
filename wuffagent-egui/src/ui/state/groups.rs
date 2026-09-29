//! Field groups for [`super::ChatApp`].
//!
//! `ChatApp` used to be one flat ~40-field struct. The fields are split into
//! small sub-structs by responsibility (one per concern) and re-embedded in
//! `ChatApp`, so each area of app state has its own cohesive unit instead of
//! a god-struct. UI modules reach a field as `self.<group>.<field>`.

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use eframe::egui;

use wuffagent_core::config::Config;
use wuffagent_core::server::ServerManager;
use wuffagent_core::tools::ToolManager;
use wuffagent_core::types::{AppEvent, AppStatus, ChatMessage};

/// App-wide service handles created at startup (server, shared connection,
/// engine, memory, MCP, tools).
pub struct CoreServices {
    /// Current chat configuration
    pub config: Config,
    /// HTTP client for the WuffAgent server
    pub server: ServerManager,
    /// Shared connection settings (URL + API key) used by every client in the
    /// app (session runtimes, bootstrap engine, non-streaming LLM adapter).
    /// One `update()` here propagates to all of them.
    pub connection: Arc<wuffagent_core::client::ConnectionSettings>,
    /// Last base_url synced to the shared connection settings (no-op guard).
    pub last_synced_base_url: String,
    /// Shared tool manager
    pub tool_manager: Arc<ToolManager>,
    /// Agent engine
    pub agent_engine: Arc<wuffagent_core::agents::AgentEngine>,
    /// Shared memory manager (single-writer discipline; all UI memory writes go
    /// through it). Shared with the agent engine and the memory tools.
    pub memory_manager: Arc<wuffagent_core::memory::MemoryManager>,
    /// Dedicated runtime for UI-triggered async memory work (maintenance pass).
    /// Wrapped in [`super::RuntimeOnThread`] so its `Drop` (a blocking wait) never
    /// runs on a thread inside another runtime's async context.
    pub memory_runtime: super::RuntimeOnThread,
    /// MCP (Model Context Protocol) manager: owns the dedicated runtime for
    /// MCP I/O and mirrors connected servers' tools into the shared registry.
    /// Its own `shutdown()` (called in `Drop`) disconnects all servers and
    /// drops that runtime on a plain thread.
    pub mcp_manager: Arc<wuffagent_core::tools::mcp::McpManager>,
}

/// Per-session runtime state + session selection + chat-input staging.
pub struct SessionState {
    /// Per-session runtime state keyed by session ID.
    pub session_store: HashMap<String, wuffagent_core::sessions::SessionRuntime>,
    /// ID of the currently selected session (None = no session selected).
    pub selected_session_id: Option<String>,
    /// Open sub-session tab ids (order = display order). A sub-session is a
    /// clean context forked from a sub-session handoff; its session file
    /// carries a `parent_session_id` back to the forking session. The tabs
    /// are global live views — they stay open while the user switches
    /// sessions (a closed tab's session remains in the session list).
    pub sub_session_tabs: Vec<String>,
    /// The tab shown in the chat area: a sub-session id, or None = the main
    /// (selected) session tab. Reset to None when the selected session
    /// changes (the user switched to another session in the sidebar).
    pub active_tab: Option<String>,
    /// Attached-but-unsent image per session (pasted or attached, not yet
    /// sent). The egui-side half of the image flow: the UI keeps the
    /// `ImageSource` for preview rendering and converts it to a `data:` URI
    /// when the message crosses into core (`QueuedMessage.image`).
    pub pending_images: HashMap<String, egui::ImageSource<'static>>,
}

/// Transient dialog/panel widgets and their visibility flags.
pub struct Dialogs {
    pub show_settings: bool,
    pub settings_dialog: Option<crate::ui::settings::SettingsDialog>,
    pub presets_dialog: Option<crate::ui::presets_dialog::PresetsDialog>,
    pub show_agent_config: bool,
    pub agent_config_dialog: Option<crate::ui::agent_config::AgentConfigDialog>,
    /// The memory panel widget (owns its own list/search/edit state).
    /// `show_panel` gates whether the window is drawn.
    pub memory_panel: crate::ui::memory_panel::MemoryPanel,
    /// The token-usage panel (floating window with an LLM token chart).
    /// `show_panel` gates whether the window is drawn; stream completions
    /// mark it dirty so the next frame picks up newly logged calls.
    pub usage_panel: crate::ui::usage_panel::UsagePanel,
    /// The fleet dashboard panel (3a: per-agent KPI cards + 30-day trend +
    /// improvement-loop state). `show` gates the window; stream completions
    /// mark it dirty so the next frame re-reads the metrics stores.
    pub dashboard: crate::ui::dashboard::FleetDashboard,
    /// The MCP panel widget (server list, add/edit, per-tool toggles).
    pub mcp_panel: crate::ui::mcp_panel::McpPanel,
    /// Pending agent improvement suggestions.
    pub improvements_panel: crate::ui::improvements::ImprovementsPanel,
}

/// Core → UI event channel (the corresponding receiver is polled by
/// `process_pending_events`).
pub struct EventRelay {
    /// Channel sender for relaying core events (EngineEvent, AppEvent) to the UI thread.
    pub pending_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    pub pending_rx: Option<mpsc::Receiver<AppEvent>>,
}

/// Server-synced context window state (remote n_ctx).
pub struct RemoteNctx {
    /// Remote n_ctx value (for remote mode).
    pub remote_n_ctx: u32,
    /// Handle for the remote n_ctx update task.
    pub remote_n_ctx_handle: Option<JoinHandle<()>>,
    /// Arc for the remote n_ctx atomic value.
    pub remote_n_ctx_arc: Option<Arc<AtomicU32>>,
}

/// Cached rendered messages (perf: avoid deep-cloning messages per frame) +
/// the status bar value.
pub struct DisplayState {
    /// Shared display snapshot of the selected session's messages. Rebuilt only
    /// when the session or its message set changes, so a per-frame redraw is an
    /// O(1) `Arc::clone` instead of a full deep clone of every message (which
    /// copies large tool outputs + base64 images and is the main lag).
    pub display_snapshot: Arc<Vec<ChatMessage>>,
    /// Session id the snapshot belongs to (None = empty).
    pub snapshot_session: Option<String>,
    /// Message count the snapshot was built from.
    pub snapshot_len: usize,
    /// Set on in-place message edits (which keep the count unchanged) to force a rebuild.
    pub display_dirty: bool,
    /// Status bar state.
    pub status: AppStatus,
}

/// Restart / auto-resume lifecycle state.
pub struct RestartState {
    /// Set when a `restart` tool run requested a relaunch (marker written + new
    /// process spawned); the window closes on the next frame.
    pub pending_restart: bool,
    /// Set once by `main` when a restart marker was found at startup; the first
    /// frame where the resumed session's runtime exists auto-sends the resume turn.
    pub pending_auto_resume: bool,
    /// Reason captured from the restart marker, used to build the resume turn.
    pub auto_resume_reason: Option<String>,
    /// Set by `main` when a restart marker was found at startup BUT its session
    /// could not be loaded — the auto-resume cannot run, so the UI shows a
    /// one-shot dismissible banner explaining why the window started empty.
    /// `(session_id, restart_reason)`. Cleared when the user dismisses it.
    pub resume_failed: Option<(String, String)>,
}
