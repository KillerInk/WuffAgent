use std::sync::mpsc;

pub mod chat;
pub mod http;
mod conversation;
pub mod persist;
pub mod sse;
pub mod trim_state;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::sessions::SessionState;
use crate::types::{Message, Usage};

/// Shared, live connection settings (base URL + API key).
///
/// One instance is shared by every `ChatClient` in the app (bootstrap engine
/// client, per-session clients, non-streaming LLM adapter) via cheap `Arc`
/// clones, so a settings/preset change propagates to all of them with a single
/// `update()` call instead of walking every client and pushing `set_url`.
#[derive(Clone)]
pub struct ConnectionSettings {
    base_url: Arc<Mutex<String>>,
    api_key: Arc<Mutex<Option<String>>>,
}

impl ConnectionSettings {
    pub fn new(base_url: &str, api_key: Option<&str>) -> Self {
        Self {
            base_url: Arc::new(Mutex::new(base_url.to_string())),
            api_key: Arc::new(Mutex::new(api_key.map(|s| s.to_string()))),
        }
    }

    pub fn base_url(&self) -> String {
        self.base_url.lock().unwrap().clone()
    }

    pub fn api_key(&self) -> Option<String> {
        self.api_key.lock().unwrap().clone()
    }

    /// Update URL and API key in place; every client sharing these settings
    /// sees the new values on its next request.
    pub fn update(&self, base_url: &str, api_key: Option<&str>) {
        *self.base_url.lock().unwrap() = base_url.to_string();
        *self.api_key.lock().unwrap() = api_key.map(|s| s.to_string());
    }
}

// Re-export key types so the public API surface is unchanged
pub use http::{
    build_request, build_stream_request, send_message, ChatRequest, ChatRequestRef, Choice,
    NonStreamResult, Response,
};

// Session persistence orchestrators moved to `crate::sessions::persist`
// (Phase 2, E2a); re-exported here so `client::{save_session, load_session, ...}`
// keep resolving for backward compatibility.
pub use crate::sessions::persist::{
    clear_save_failure, enqueue_save_failure, has_save_failure, load_session,
    retry_pending_saves, save_session,
};
pub use sse::{
    add_streaming_messages, looks_like_complete_json, process_sse_line, stream_message,
    ToolCallTracker,
};
// Re-export trimming helpers for backward compatibility
pub use crate::trimming::{
    estimate_tokens, message_char_count, ContextOverflow, ContextTrimming,
};
pub use http::{parse_props_n_ctx, strip_think_tags};

/// Backward-compat wrapper: delegates to `ContextTrimming::trim_to_token_budget`.
pub fn trim_to_token_budget(
    conversation: &std::sync::Arc<std::sync::Mutex<Vec<crate::types::Message>>>,
    target_tokens: usize,
) -> usize {
    let trimming = ContextTrimming::new();
    let config = crate::trimming::TrimConfig::default();
    trimming.trim_to_token_budget(&mut conversation.lock().unwrap(), target_tokens, &config)
}

/// Backward-compat wrapper: delegates to `ContextTrimming::trim_messages`.
pub fn trim_to_token_budget_messages(
    messages: &mut Vec<crate::types::Message>,
    target_tokens: usize,
) -> usize {
    let trimming = ContextTrimming::new();
    let config = crate::trimming::TrimConfig::default();
    trimming.trim_messages(messages, target_tokens, &config)
}

/// Backward-compat wrapper: estimates conversation tokens via `message_char_count`.
pub fn estimate_conversation_tokens(
    conversation: &std::sync::Arc<std::sync::Mutex<Vec<crate::types::Message>>>,
) -> usize {
    message_char_count(&conversation.lock().unwrap())
}

/// Parse a server `exceed_context_size_error` (HTTP 400) into the reported
/// prompt size and context window, so the caller can force-trim to fit and
/// retry. Returns `None` for any other error.
pub fn parse_context_overflow(err: &Error) -> Option<ContextOverflow> {
    let Error::Http(msg) = err else {
        return None;
    };
    crate::trimming::parse_context_overflow_msg(msg)
}

pub struct ChatClient {
    /// Connection settings (base URL + API key). All app-level clients share
    /// one `ConnectionSettings` instance, so a settings/preset change
    /// propagates to every client without cloning or per-client `set_url`
    /// pushes. `new()` creates a private instance for standalone clients.
    settings: ConnectionSettings,
    /// Reasoning effort level for reasoning models (see `types::ReasoningEffort`
    /// for the wire mapping; Off disables Qwen3 thinking via `enable_thinking`).
    ///
    /// Interior-mutable: the session's SHARED client is re-synced through an
    /// `Arc` when the UI's mode changes (`sync_session_meta`). `ChatClient::clone`
    /// still COPIES the value into a fresh mutex, so a per-run clone's forced
    /// level stays isolated from the shared session client.
    reasoning_effort: Mutex<crate::types::ReasoningEffort>,
    /// The per-session state (conversation buffer + session identity + save
    /// queue): a cheaply cloneable handle, so a cloned client shares the SAME
    /// conversation AND session identity (previously only the conversation
    /// was shared; the identity fields were value-copied and could drift).
    /// See `sessions::state`.
    session: SessionState,
    /// Shared so `ChatClient::clone` is a cheap pointer bump instead of
    /// deep-copying the reqwest connection pool + TLS state.
    http_client: Arc<reqwest::Client>,
    /// Dedicated client for SSE streaming requests.
    ///
    /// Uses connect + read (idle) timeouts instead of a *total* request
    /// timeout: a total timeout cancels long local-model generations
    /// mid-stream (reqwest surfaces the cancellation as
    /// "error decoding response body"). The read timeout only fires when
    /// the connection stays silent for the given duration, so streams of
    /// arbitrary length survive as long as tokens keep flowing.
    stream_http_client: Arc<reqwest::Client>,
    max_messages: usize,
    /// Context window size in tokens (0 = use server default).
    ///
    /// Shared interior-mutable so the effective n_ctx can be updated in place
    /// (e.g. from the remote server's /props) without cloning the client:
    /// every holder of a clone reads/writes the same value.
    n_ctx: Arc<std::sync::atomic::AtomicU32>,
    /// Calibrated chars-per-token ratio × 100 (350 = 3.5 chars/token).
    ///
    /// Updated after every successful round from the server-reported
    /// `usage.prompt_tokens`, so the estimate converges on the actual
    /// tokenizer / content mix instead of a static constant — which was
    /// either too low (trim destroyed most of the window on English prose)
    /// or too high (the server rejected the request first).
    chars_per_token_x100: Arc<std::sync::atomic::AtomicU32>,
    /// Estimator char count of the last request's prompt, paired with
    /// `usage.prompt_tokens` by [`ChatClient::calibrate_from_usage`].
    last_prompt_chars: Arc<Mutex<usize>>,
    /// Channel to send tool execution events to the UI.
    tool_event_tx: Arc<Mutex<Option<mpsc::Sender<crate::types::AppEvent>>>>,
    /// Token-usage recorder: appends one JSONL line per completed LLM call
    /// (default `~/.wuffagent/usage.jsonl`; tests may inject a temp-path
    /// recorder via `set_usage_recorder`). Writing is best-effort — a log
    /// failure must never break the chat.
    usage_recorder: Arc<crate::usage::recorder::UsageRecorder>,
    /// Name of the agent about to make LLM calls on this client. Set by
    /// `Agent::builder` before each agent run; agents within a session run
    /// sequentially, so the stamp is current at request time. Shared
    /// interior-mutable like the other per-client fields, since the client
    /// handle is cloned and shared.
    agent_name: Arc<Mutex<String>>,
    /// (trigger%, target%) of the context window for the agent about to run
    /// on this client. Stamped by `Agent::builder` from the agent's
    /// `TrimConfig` before each run (per-run state like `agent_name` — the
    /// client handle is shared across agents). Defaults to the proven 90/50
    /// values until an agent stamps its own (S3, autoplans/
    /// context-rot-prevention.md: the cliff is now tunable per agent).
    trim_pcts: Arc<Mutex<(u64, u64)>>,
    /// 1e: the run id to stamp on usage-log lines written by this client
    /// (`None` = no run active / cleared). Set by the agent loop at run
    /// start and cleared at run end — the same per-run stamping pattern as
    /// `agent_name`. The join key that ties usage.jsonl lines to the
    /// metrics Run/Trim lines.
    run_id: Arc<Mutex<Option<String>>>,
    /// 1b: the model name of the most recent recorded LLM call (None until
    /// the first call reports one) — read by the agent's run-completion hook
    /// for the Run line's model label. Safe: clients are per-session and
    /// agents run sequentially per session (same assumption as `agent_name`);
    /// clones of the client share the stamp (shared interior state).
    last_model: Arc<Mutex<Option<String>>>,
}

impl Clone for ChatClient {
    fn clone(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            // Copy the current value into a FRESH mutex (not an Arc clone):
            // a per-run clone's forced level must stay isolated from the
            // shared session client, while the conversation + session
            // identity below remain the same shared store.
            reasoning_effort: Mutex::new(self.reasoning_effort()),
            session: self.session.clone(),
            http_client: self.http_client.clone(),
            stream_http_client: self.stream_http_client.clone(),
            max_messages: self.max_messages,
            n_ctx: self.n_ctx.clone(),
            chars_per_token_x100: self.chars_per_token_x100.clone(),
            last_prompt_chars: self.last_prompt_chars.clone(),
            tool_event_tx: self.tool_event_tx.clone(),
            usage_recorder: self.usage_recorder.clone(),
            agent_name: self.agent_name.clone(),
            trim_pcts: self.trim_pcts.clone(),
            run_id: self.run_id.clone(),
            last_model: self.last_model.clone(),
        }
    }
}

impl ChatClient {
    /// Default HTTP timeout of 5 minutes.
    const DEFAULT_TIMEOUT_SECS: u64 = 300;

    /// Default chars-per-token ratio (×100) before the server has reported
    /// any usage: 3.5 chars/token, typical for BPE tokenizers on
    /// English/code content.
    const DEFAULT_CHARS_PER_TOKEN_X100: u32 = 350;

    /// Percentage of the context window at which trimming kicks in (×100).
    /// While the estimated conversation stays below this threshold, the full
    /// history is kept untouched.
    const TRIM_TRIGGER_PCT: u64 = 90;

    /// Percentage of the context window to trim DOWN TO once the trigger
    /// threshold is exceeded (×100). Trimming does not stop at "just below
    /// the limit" — it drops the conversation well below (to 50%) so the
    /// following rounds have headroom before the trigger fires again.
    const TRIM_TARGET_PCT: u64 = 50;

    pub fn new(base_url: &str) -> Self {
        Self::new_with_timeout(base_url, Self::DEFAULT_TIMEOUT_SECS)
    }

    /// Create a ChatClient with a custom HTTP timeout (in seconds).
    pub fn new_with_timeout(base_url: &str, timeout_secs: u64) -> Self {
        let d = std::time::Duration::from_secs(timeout_secs);
        Self {
            settings: ConnectionSettings::new(base_url, None),
            reasoning_effort: Mutex::new(crate::types::ReasoningEffort::default()),
            session: SessionState::default(),
            http_client: Arc::new({
                // Non-streaming calls: total request timeout is fine
                // (short round-trips).
                reqwest::Client::builder()
                    .timeout(d)
                    .pool_max_idle_per_host(10)
                    .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
                    .build()
                    .unwrap()
            }),
            stream_http_client: Arc::new({
                // Streaming calls: NO total timeout — generation can
                // legitimately run for many minutes. The read timeout
                // acts as an idle/dead-connection guard instead.
                reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(30))
                    .read_timeout(d)
                    .pool_max_idle_per_host(10)
                    .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
                    .build()
                    .unwrap()
            }),
            max_messages: 100,
            n_ctx: Arc::new(std::sync::atomic::AtomicU32::new(4096)),
            chars_per_token_x100: Arc::new(std::sync::atomic::AtomicU32::new(
                Self::DEFAULT_CHARS_PER_TOKEN_X100,
            )),
            last_prompt_chars: Arc::new(Mutex::new(0)),
            tool_event_tx: Arc::new(Mutex::new(None)),
            usage_recorder: Arc::new(crate::usage::recorder::UsageRecorder::default_recorder()),
            agent_name: Arc::new(Mutex::new("chat".to_string())),
            run_id: Arc::new(Mutex::new(None)),
            last_model: Arc::new(Mutex::new(None)),
            trim_pcts: Arc::new(Mutex::new((
                Self::TRIM_TRIGGER_PCT,
                Self::TRIM_TARGET_PCT,
            ))),
        }
    }

    /// Create a client that reads its base URL and API key from the shared
    /// `settings` instance. All app-level clients should be built this way
    /// (from the single bootstrap `ConnectionSettings`) so settings/preset
    /// changes reach every client.
    pub fn from_settings(settings: ConnectionSettings) -> Self {
        let mut client = Self::new(&settings.base_url());
        client.settings = settings;
        client
    }

    /// Like [`from_settings`], but with a custom total timeout (in seconds)
    /// for non-streaming requests (streaming requests keep using it as the
    /// read/idle timeout).
    pub fn from_settings_with_timeout(settings: ConnectionSettings, timeout_secs: u64) -> Self {
        let mut client = Self::new_with_timeout(&settings.base_url(), timeout_secs);
        client.settings = settings;
        client
    }

    /// 2b: a fresh client that shares this client's connection settings
    /// (base URL / API key) and context window but starts an EMPTY
    /// conversation (a new session id). The eval harness uses it so a
    /// headless run's messages don't pollute the live session's
    /// conversation. The `LlmClient` transport is separate — the caller
    /// passes the shared one — so this only isolates the conversation store.
    pub fn fresh(&self) -> ChatClient {
        let mut client = Self::from_settings(self.settings.clone());
        client.n_ctx = self.n_ctx.clone();
        client
    }

    pub fn set_tool_event_sender(&self, tx: mpsc::Sender<crate::types::AppEvent>) {
        *self.tool_event_tx.lock().unwrap() = Some(tx);
    }

    /// Stamp the agent name on usage-log lines written by this client.
    /// Called by `Agent::builder` before each agent run (agents within a session
    /// run sequentially, so the name is current when the LLM call happens).
    pub fn set_agent_name(&self, name: &str) {
        *self.agent_name.lock().unwrap() = name.to_string();
    }

    /// 1e: stamp the run id on usage-log lines written by this client
    /// (the join key to the metrics Run/Trim lines). `None` clears the
    /// stamp — the agent loop sets it at run start and clears it at run
    /// end (same per-run pattern as `set_agent_name`).
    pub fn set_run_id(&self, run_id: Option<&str>) {
        *self.run_id.lock().unwrap() = run_id.map(str::to_string);
    }

    /// 1b: the model name of the most recent recorded LLM call ("" when the
    /// server reported none yet).
    pub fn last_model(&self) -> String {
        self.last_model.lock().unwrap().clone().unwrap_or_default()
    }

    /// Stamp this agent's trim thresholds (percent of the n_ctx window:
    /// trigger / target) from its `TrimConfig`. Called by `Agent::builder`
    /// before each agent run, like [`Self::set_agent_name`]; the client then
    /// derives its char budgets from these values
    /// (`trim_trigger_chars` / `trim_target_chars`).
    pub fn set_trim_pcts(&self, trigger_pct: u64, target_pct: u64) {
        *self.trim_pcts.lock().unwrap() = (trigger_pct, target_pct);
    }

    /// The stamped (trigger%, target%) pair (defaults: the proven 90/50).
    fn trim_pcts(&self) -> (u64, u64) {
        *self.trim_pcts.lock().unwrap()
    }

    /// Inject a custom usage recorder (mainly for tests: point the log at a
    /// temp file instead of `~/.wuffagent/usage.jsonl`).
    pub fn set_usage_recorder(&mut self, recorder: Arc<crate::usage::recorder::UsageRecorder>) {
        self.usage_recorder = recorder;
    }

    /// Append one completed LLM call to the usage log. Best-effort: a log
    /// failure must never break the chat (the recorder degrades to a
    /// `tracing` log). `None` usage (backend reported nothing) means there is
    /// no server-reported count to store, so nothing is logged.
    ///
    /// `tool_calls` is how many tool calls the assistant issued in this call;
    /// `thinking_chars` is the character count of its thinking/reasoning text.
    fn record_usage(
        &self,
        usage: Option<&Usage>,
        model: Option<&str>,
        tool_calls: u32,
        thinking_chars: u64,
    ) {
        let Some(usage) = usage else {
            return;
        };
        let session_id = self.session.session_id().unwrap_or_default();
        let agent = self.agent_name.lock().unwrap().clone();
        let run_id = self.run_id.lock().unwrap().clone().unwrap_or_default();
        // 1b: remember the last model name for the run line's model label.
        if let Some(m) = model {
            *self.last_model.lock().unwrap() = Some(m.to_string());
        }
        self.usage_recorder
            .record(&crate::usage::recorder::UsageEntry {
                ts: chrono::Utc::now(),
                session_id,
                agent,
                model: model.unwrap_or("unknown").to_string(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                tool_calls,
                thinking_chars,
                run_id,
                v: 1,
            });
    }

    pub fn set_max_messages(&mut self, max_messages: usize) {
        self.max_messages = max_messages;
    }

    /// Update the effective n_ctx in place. Takes `&self` (interior mutability)
    /// so callers can sync the server's reported context size on a shared client
    /// without cloning it — every clone sees the new value immediately.
    pub fn set_n_ctx(&self, n_ctx: u32) {
        self.n_ctx
            .store(n_ctx, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn n_ctx(&self) -> u32 {
        self.n_ctx.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Update the server URL in place. Works on a shared `Arc<ChatClient>`
    /// because the URL is interior-mutable, so a preset/settings change can be
    /// pushed to every live client (session runtimes, bootstrap engine, LLM
    /// adapter) without rebuilding them.
    pub fn set_url(&self, url: &str) {
        *self.settings.base_url.lock().unwrap() = url.to_string();
    }

    pub fn base_url(&self) -> String {
        self.settings.base_url()
    }

    /// Clone the current base URL into an owned `String`. Used at request-build
    /// sites so the `MutexGuard` is dropped before any `.await` (a guard is not
    /// `Send` and must not be held across an await boundary).
    fn url(&self) -> String {
        self.settings.base_url()
    }

    pub fn set_api_key(&self, key: Option<&str>) {
        *self.settings.api_key.lock().unwrap() = key.map(|s| s.to_string());
    }

    /// Current API key (owned clone — the key lives in shared interior-mutable
    /// settings, so no `&str` can borrow out of the guard).
    pub fn api_key(&self) -> Option<String> {
        self.settings.api_key()
    }

    pub fn set_system_prompt(&self, prompt: &str) {
        self.session.set_system_prompt(prompt);
    }

    /// The system prompt (owned clone — it lives in the shared session state).
    pub fn system_prompt(&self) -> String {
        self.session.system_prompt()
    }

    pub fn set_reasoning_effort(&self, effort: crate::types::ReasoningEffort) {
        *self.reasoning_effort.lock().unwrap() = effort;
    }

    pub fn reasoning_effort(&self) -> crate::types::ReasoningEffort {
        *self.reasoning_effort.lock().unwrap()
    }

    /// Set the per-session UI selections persisted with the session file
    /// (chosen agent profile + reasoning-effort mode). Call whenever the UI
    /// changes either selection or a session is loaded.
    pub fn set_session_meta(&self, meta: crate::sessions::SessionMeta) {
        self.session.set_session_meta(meta);
    }

    /// The per-session UI selections (owned clone — they live in the shared
    /// session state).
    pub fn session_meta(&self) -> crate::sessions::SessionMeta {
        self.session.session_meta()
    }

    /// The shared per-session state (conversation buffer + session identity +
    /// save queue). Cloning a client shares the same state.
    pub fn session(&self) -> &SessionState {
        &self.session
    }

    pub fn conversation(&self) -> &Arc<Mutex<Vec<Message>>> {
        self.session.conversation()
    }

    pub fn clear_history(&self) {
        conversation::clear_history(self.session.conversation());
    }

    pub fn clear_session_messages(&self) {
        conversation::clear_session_messages(self.session.conversation(), &|| {
            save_session(&self.session)
        });
    }

    /// Bind this client to a session (id + file directory). Takes `&self` —
    /// the binding lives in the shared session state.
    pub fn set_session(&self, session_id: Option<String>, session_dir: PathBuf) {
        self.session.set_session_id(session_id);
        self.session.set_session_dir(session_dir);
    }

    /// Clear the current session (set session_id to None) and wipe the
    /// in-memory conversation. Call this when a session is deleted so that a
    /// subsequent save cannot resurrect the deleted session file from a
    /// stale in-memory conversation buffer.
    pub fn clear_session(&self) {
        self.session.set_session_id(None);
        self.session.conversation().lock().unwrap().clear();
    }

    pub fn set_encryption_key(&self, key: Option<[u8; 32]>) {
        self.session.set_encryption_key(key);
    }

    /// The session file directory (owned clone — it lives in the shared
    /// session state).
    pub fn session_dir(&self) -> PathBuf {
        self.session.session_dir()
    }

    /// The active session id (owned clone — it lives in the shared session
    /// state).
    pub fn session_id(&self) -> Option<String> {
        self.session.session_id()
    }

}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Cancelled")]
    Cancelled,
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::Http(err.to_string())
    }
}

#[cfg(test)]
mod tests;
