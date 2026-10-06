use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

// ─── Error Types ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    NotFound(String),
    #[error("Tool execution failed: {0}")]
    Execution(String),
    #[error("Invalid parameters: {0}")]
    InvalidParams(String),
    #[error("Plugin load error: {0}")]
    PluginLoad(String),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ToolError {
    /// Returns true if this error is likely fixable by the planner
    /// regenerating the task with correct parameters.
    pub fn is_fixable(&self) -> bool {
        matches!(self, ToolError::InvalidParams(_))
    }
}

pub type ToolResult<T> = Result<T, ToolError>;

// ─── Schema Types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub type_name: String,
    pub description: String,
    pub nullable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, FieldSchema>>,
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<JsonSchema>,
}

// ─── Tool Definition (for AI function calling) ─────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(rename = "function")]
    pub function: ToolFunctionSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolFunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchema,
}

// ─── Tool Parameters and Output ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolParams {
    pub values: HashMap<String, serde_json::Value>,
}

impl ToolParams {
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.values
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

impl Default for ToolParams {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ToolOutput {
    Success(serde_json::Value),
    Error(String),
}

impl ToolOutput {
    pub fn success(value: impl Into<serde_json::Value>) -> Self {
        ToolOutput::Success(value.into())
    }

    pub fn error(msg: impl Into<String>) -> Self {
        ToolOutput::Error(msg.into())
    }
}

/// Progress sink passed to tools while they execute.
///
/// Tools with incremental output (the shell streams command output lines)
/// override [`Tool::execute_with_progress`] and call [`ToolProgress::report`]
/// as work happens. The agent relays each report to the UI, which updates
/// the live tool card, so the user watches long-running commands instead of
/// a frozen "running" label.
///
/// Reports use "latest tail" semantics: the UI REPLACES its displayed text
/// with each report, so tools should send a small rolling window (a few
/// lines), never the full transcript.
#[derive(Clone, Default)]
pub struct ToolProgress {
    pub on_progress: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
}

impl ToolProgress {
    /// A progress sink that swallows all reports (for callers that don't
    /// care about live output).
    pub fn none() -> Self {
        Self::default()
    }

    /// Report a progress text snapshot (no-op when there is no sink).
    pub fn report(&self, text: &str) {
        if let Some(f) = &self.on_progress {
            f(text);
        }
    }
}

// ─── Tool Metadata ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub dependencies: Vec<String>,
}

// ─── Core Tool Trait ────────────────────────────────────────────────────────

pub trait Tool: Send + Sync {
    /// Human-readable name of the tool.
    fn name(&self) -> &str;

    /// Description used for AI function-calling schemas.
    fn description(&self) -> &str;

    /// JSON schema describing the expected input parameters.
    fn parameters_schema(&self) -> ToolSchema;

    /// Execute the tool with the given parameters.
    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput>;

    /// Execute the tool with the given parameters, optionally reporting
    /// incremental progress (e.g. the shell streams output lines).
    ///
    /// Default implementation ignores the sink and delegates to
    /// [`Tool::execute`]; only tools with live output (the shell) override
    /// this.
    fn execute_with_progress(
        &self,
        params: ToolParams,
        progress: &ToolProgress,
    ) -> ToolResult<ToolOutput> {
        let _ = progress;
        self.execute(params)
    }

    /// Execute the tool with the given parameters, optionally reporting
    /// incremental progress, plus a per-call cancel token (the live tool
    /// card's Stop button).
    ///
    /// Default implementation ignores the token and delegates to
    /// [`Tool::execute_with_progress`]; only tools with a killable
    /// long-running workload (the shell) override this. A cancelled token
    /// does NOT stop the default implementation itself — the caller's
    /// `select!` on the token is what makes the agent loop move on.
    fn execute_with_cancel(
        &self,
        params: ToolParams,
        progress: &ToolProgress,
        cancel: &CancellationToken,
    ) -> ToolResult<ToolOutput> {
        let _ = cancel;
        self.execute_with_progress(params, progress)
    }
}

// ─── FFI Plugin ABI ─────────────────────────────────────────────────────────

/// The version of the WuffAgent plugin ABI.
///
/// Plugins MUST export `wuff_tool_abi_version() -> u32` returning this value
/// (compiled in from the `wuffagent-core` they link against). The loader
/// rejects a plugin whose value differs: between ABI versions the vtable
/// layouts of `Tool`/`ToolSchema`/`ToolParams` etc. can change, and loading a
/// stale DLL would then corrupt the host's heap (segfault / access violation
/// deep in unrelated code) instead of failing cleanly.
pub const PLUGIN_ABI_VERSION: u32 = 1;

/// Opaque FFI-safe wrapper for passing trait objects across the plugin boundary.
/// Plugins box their Tool and return this wrapper; the host converts it back.
/// Uses two raw pointers (vtable + data) to represent the wide *mut dyn Tool pointer.
#[repr(C)]
pub struct PluginTool {
    vtable: *const (),
    data: *mut std::ffi::c_void,
}

// SAFETY: PluginTool only stores pointers and is passed by value across FFI.
// The host is responsible for ensuring the underlying Tool lives long enough.
unsafe impl Send for PluginTool {}
// SAFETY: Tool requires Send + Sync, and PluginTool merely wraps a Tool pointer.
unsafe impl Sync for PluginTool {}

impl PluginTool {
    /// Create a new PluginTool from a Box<dyn Tool>.
    pub fn from_box(tool: Box<dyn Tool>) -> Self {
        // SAFETY: We split the wide pointer into its vtable and data components.
        // Box::into_raw gives us *mut dyn Tool (128 bits on 64-bit).
        // We use ptr::from_mut to get the raw components.
        let raw: *mut dyn Tool = Box::into_raw(tool);
        // SAFETY: A wide pointer is exactly two pointers (vtable, data) in memory.
        let (vtable, data) = unsafe {
            let slice: [*const (); 2] = std::mem::transmute(raw);
            (slice[0], slice[1] as *mut std::ffi::c_void)
        };
        Self { vtable, data }
    }

    /// Convert this wrapper back into an owned Box<dyn Tool>.
    /// # Safety
    /// The caller must ensure the PluginTool was created via `from_box`
    /// and has not been consumed yet.
    pub unsafe fn into_box(self) -> Box<dyn Tool> {
        // SAFETY: We reconstruct the wide pointer from its vtable and data components.
        let raw: *mut dyn Tool = std::mem::transmute([self.vtable, self.data as *const ()]);
        Box::from_raw(raw)
    }
}

impl Drop for PluginTool {
    fn drop(&mut self) {
        // SAFETY: We own the pointer and are dropping it here.
        unsafe {
            let raw: *mut dyn Tool = std::mem::transmute([self.vtable, self.data as *const ()]);
            drop(Box::from_raw(raw));
        }
    }
}

// ─── Host API (plugin → app, optional) ──────────────────────────────────────

/// Version of the optional host-API vtable ([`HostApi`]).
///
/// Separate from [`PLUGIN_ABI_VERSION`]: the 3-symbol tool ABI is unchanged
/// (old plugins keep loading), the host-API export `wuff_tool_host_api` is
/// opt-in. Bump when the table's layout or a signature changes; a plugin that
/// sees an unrecognized version must degrade (e.g. to send-only mode).
pub const HOST_API_VERSION: u32 = 1;

/// Event callback the plugin registers via [`HostApi::register_event_callback`].
///
/// Invoked on the host's UI thread for pipeline events of ANY session.
/// Kinds: 0 = stream-chunk text; 1 = full final content; 2 = error message;
/// 3 = round-complete marker (payload empty). `session_id`/`payload` are
/// host-owned byte slices, valid for the call only — the callback must copy
/// what it needs.
///
/// Contract: MUST be fast and non-blocking (it runs while the UI frame is
/// drawn) — the implementation should just forward into an mpsc.
pub type HostEventCallback = extern "C" fn(
    kind: u32,
    session_id: *const u8,
    sid_len: usize,
    payload: *const u8,
    payload_len: usize,
    user_data: *mut std::ffi::c_void,
);

/// C-compatible fn-pointer table the host hands to a plugin that exports the
/// optional `wuff_tool_host_api(host_api: *const HostApi)` symbol.
///
/// `#[repr(C)]`, no Rust types by value. The host sets it once (before the
/// first plugin load) and the pointer stays valid for the process lifetime
/// (plugins are never unloaded).
///
/// Blocking contract: `inject_user_message` / `create_session` /
/// `resolve_session` / `session_count` / `get_session` are command+wait
/// (≤ 5 s timeout) — safe from the plugin's poller thread and from tool
/// execution, but MUST NOT be called from the host's UI thread (deadlock).
/// `register_event_callback` is a plain slot store and callable anywhere.
#[repr(C)]
pub struct HostApi {
    /// [`HOST_API_VERSION`] the host was built against; the plugin must check
    /// it and degrade on mismatch.
    pub version: u32,
    /// Enqueue a user message (UTF-8 bytes) for the session `session_id`
    /// (UTF-8 bytes). Auto-creates the session if missing. Returns `false` on
    /// timeout or when the app is shutting down.
    pub inject_user_message:
        extern "C" fn(session_id: *const u8, session_len: usize, text: *const u8, text_len: usize)
            -> bool,
    /// Create a named session; writes the new id (NUL-terminated) into
    /// `out_id`/`out_cap`. Returns `false` on failure or when the buffer is
    /// too small (`out_cap < 64`).
    pub create_session:
        extern "C" fn(name: *const u8, name_len: usize, out_id: *mut u8, out_cap: usize) -> bool,
    /// Resolve a session by exact id, or by name (case-insensitive, first
    /// match wins). Writes the id (NUL-terminated) into `out_id`/`out_cap`.
    pub resolve_session:
        extern "C" fn(query: *const u8, query_len: usize, out_id: *mut u8, out_cap: usize) -> bool,
    /// Switch the UI to the session `session_id` (UTF-8 bytes) — the desktop
    /// follows along when the plugin joins/switches sessions. Returns `false`
    /// when the session is unknown or on timeout.
    pub switch_session: extern "C" fn(session_id: *const u8, session_len: usize) -> bool,
    /// Number of sessions in the store.
    pub session_count: extern "C" fn() -> usize,
    /// Write session `index`'s id and name (both NUL-terminated) into the two
    /// out buffers (independent caps). Returns `false` if `index` is out of
    /// range.
    pub get_session: extern "C" fn(
        index: usize,
        out_id: *mut u8,
        id_cap: usize,
        out_name: *mut u8,
        name_cap: usize,
    ) -> bool,
    /// Register the event callback (single slot; re-registration replaces the
    /// previous one). `user_data` is passed back on every event.
    pub register_event_callback:
        extern "C" fn(cb: HostEventCallback, user_data: *mut std::ffi::c_void),
}

// SAFETY: `HostApi` is a plain `#[repr(C)]` aggregate of a `u32` and
// `extern "C"` fn pointers — all `Send + Sync` — with no interior mutability.
unsafe impl Send for HostApi {}
unsafe impl Sync for HostApi {}

impl HostApi {
    /// Plugin-side version check for the vtable pointer the host handed over
    /// (dual-link case: a statically linked plugin compares the host's
    /// `version` field against its own copy of [`HOST_API_VERSION`]).
    ///
    /// Returns `None` when the pointer is null or the version is not
    /// recognized — a plugin should then degrade (e.g. send-only) instead of
    /// calling through an unknown layout.
    pub fn validate(host_api: *const HostApi) -> Option<&'static HostApi> {
        if host_api.is_null() {
            return None;
        }
        // SAFETY: the host keeps the table alive for the process lifetime
        // (see the struct docs), so a pointer obtained at load time stays
        // valid for every later plugin call.
        let api = unsafe { &*host_api };
        (api.version == HOST_API_VERSION).then_some(api)
    }
}

// ─── Logger Trait ───────────────────────────────────────────────────────────

pub trait ToolLogger: Send + Sync {
    fn log_tool_call(&self, tool_name: &str, params: &ToolParams);
    fn log_tool_result(&self, tool_name: &str, result: &ToolOutput);
    fn log_tool_error(&self, tool_name: &str, error: &ToolError);
    fn log_plugin_load(&self, path: &std::path::Path, metadata: &ToolMetadata);
    fn log_plugin_unload(&self, tool_name: &str);
}

/// Default logger backed by `tracing`.
#[derive(Clone)]
pub struct TracingToolLogger;

impl ToolLogger for TracingToolLogger {
    fn log_tool_call(&self, tool_name: &str, params: &ToolParams) {
        tracing::info!(tool = tool_name, params = ?params, "Tool called");
    }

    fn log_tool_result(&self, tool_name: &str, result: &ToolOutput) {
        match result {
            ToolOutput::Success(_) => {
                tracing::info!(tool = tool_name, "Tool succeeded");
            }
            ToolOutput::Error(msg) => {
                tracing::warn!(tool = tool_name, error = %msg, "Tool returned error output");
            }
        }
    }

    fn log_tool_error(&self, tool_name: &str, error: &ToolError) {
        tracing::error!(tool = tool_name, error = %error, "Tool execution failed");
    }

    fn log_plugin_load(&self, path: &std::path::Path, metadata: &ToolMetadata) {
        tracing::info!(
            plugin = %path.display(),
            name = metadata.name,
            version = metadata.version,
            "Plugin loaded"
        );
    }

    fn log_plugin_unload(&self, tool_name: &str) {
        tracing::info!(tool = tool_name, "Plugin unloaded");
    }
}

impl fmt::Display for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolOutput::Success(v) => match v {
                // A bare string is rendered verbatim (no JSON quoting or
                // escaping) so the model sees exactly the bytes the tool
                // produced — content copied from such a result can be
                // round-tripped into an apply_diff SEARCH block unmodified.
                // (JSON objects are still rendered as compact JSON.)
                serde_json::Value::String(s) => write!(f, "{s}"),
                other => write!(f, "{}", other),
            },
            ToolOutput::Error(e) => write!(f, "ERROR: {}", e),
        }
    }
}
