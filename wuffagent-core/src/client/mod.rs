use std::sync::mpsc;

pub mod chat;
pub mod http;
pub mod persist;
pub mod session;
pub mod sse;
pub mod trim_state;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    build_request, build_stream_request, send_message, ChatRequest, Choice, NonStreamResult,
    Response,
};
pub use session::{
    clear_history, clear_save_failure, clear_session_messages, enqueue_save_failure,
    has_save_failure, load_session, retry_pending_saves, save_session, trim_conversation,
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

#[derive(Clone)]
pub struct ChatClient {
    /// Connection settings (base URL + API key). All app-level clients share
    /// one `ConnectionSettings` instance, so a settings/preset change
    /// propagates to every client without cloning or per-client `set_url`
    /// pushes. `new()` creates a private instance for standalone clients.
    settings: ConnectionSettings,
    system_prompt: String,
    /// Reasoning effort level for reasoning models (see `types::ReasoningEffort`
    /// for the wire mapping; Off disables Qwen3 thinking via `enable_thinking`).
    reasoning_effort: crate::types::ReasoningEffort,
    conversation: Arc<Mutex<Vec<Message>>>,
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
    session_id: Option<String>,
    session_dir: PathBuf,
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
    /// Queue of pending save operations when a save fails.
    save_queue: Arc<Mutex<VecDeque<()>>>,
    /// Whether a save failure notification should be shown in the UI.
    save_failed: Arc<Mutex<bool>>,
    /// Encryption key for session files (32 bytes for ChaCha20Poly1305).
    encryption_key: Option<[u8; 32]>,
    /// Channel to send tool execution events to the UI.
    tool_event_tx: Arc<Mutex<Option<mpsc::Sender<crate::types::AppEvent>>>>,
    /// Token-usage recorder: appends one JSONL line per completed LLM call
    /// (default `~/.wuffagent/usage.jsonl`; tests may inject a temp-path
    /// recorder via `set_usage_recorder`). Writing is best-effort — a log
    /// failure must never break the chat.
    usage_recorder: Arc<crate::usage::recorder::UsageRecorder>,
    /// Name of the agent about to make LLM calls on this client. Set by
    /// `Agent::new` before each agent run; agents within a session run
    /// sequentially, so the stamp is current at request time. Shared
    /// interior-mutable like the other per-client fields, since the client
    /// handle is cloned and shared.
    agent_name: Arc<Mutex<String>>,
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
            system_prompt: String::new(),
            reasoning_effort: crate::types::ReasoningEffort::default(),
            conversation: Arc::new(Mutex::new(Vec::new())),
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
            session_id: None,
            session_dir: PathBuf::new(),
            max_messages: 100,
            n_ctx: Arc::new(std::sync::atomic::AtomicU32::new(4096)),
            chars_per_token_x100: Arc::new(std::sync::atomic::AtomicU32::new(
                Self::DEFAULT_CHARS_PER_TOKEN_X100,
            )),
            last_prompt_chars: Arc::new(Mutex::new(0)),
            save_queue: Arc::new(Mutex::new(VecDeque::new())),
            save_failed: Arc::new(Mutex::new(false)),
            encryption_key: None,
            tool_event_tx: Arc::new(Mutex::new(None)),
            usage_recorder: Arc::new(crate::usage::recorder::UsageRecorder::default_recorder()),
            agent_name: Arc::new(Mutex::new("chat".to_string())),
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

    pub fn set_tool_event_sender(&self, tx: mpsc::Sender<crate::types::AppEvent>) {
        *self.tool_event_tx.lock().unwrap() = Some(tx);
    }

    /// Stamp the agent name on usage-log lines written by this client.
    /// Called by `Agent::new` before each agent run (agents within a session
    /// run sequentially, so the name is current when the LLM call happens).
    pub fn set_agent_name(&self, name: &str) {
        *self.agent_name.lock().unwrap() = name.to_string();
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
        let session_id = self.session_id.clone().unwrap_or_default();
        let agent = self.agent_name.lock().unwrap().clone();
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

    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_prompt = prompt.to_string();
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn set_reasoning_effort(&mut self, effort: crate::types::ReasoningEffort) {
        self.reasoning_effort = effort;
    }

    pub fn reasoning_effort(&self) -> crate::types::ReasoningEffort {
        self.reasoning_effort
    }

    pub fn conversation(&self) -> &Arc<Mutex<Vec<Message>>> {
        &self.conversation
    }

    pub fn clear_history(&self) {
        session::clear_history(&self.conversation);
    }

    pub fn clear_session_messages(&mut self) {
        session::clear_session_messages(&self.conversation, &|| {
            save_session(
                self.session_id.as_deref(),
                &self.session_dir,
                &self.conversation,
                &self.system_prompt,
                self.encryption_key.as_ref(),
                &self.save_queue,
                &self.save_failed,
            )
        });
    }


    pub fn set_session(&mut self, session_id: Option<String>, session_dir: PathBuf) {
        self.session_id = session_id;
        self.session_dir = session_dir;
    }

    /// Clear the current session (set session_id to None) and wipe the
    /// in-memory conversation. Call this when a session is deleted so that a
    /// subsequent save cannot resurrect the deleted session file from a
    /// stale in-memory conversation buffer.
    pub fn clear_session(&mut self) {
        self.session_id = None;
        self.conversation.lock().unwrap().clear();
    }

    pub fn set_encryption_key(&mut self, key: Option<[u8; 32]>) {
        self.encryption_key = key;
    }

    pub fn session_dir(&self) -> &PathBuf {
        &self.session_dir
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
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
