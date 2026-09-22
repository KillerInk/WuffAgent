use std::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub mod http;
pub mod session;
pub mod sse;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::types::{Message, ToolCall, Usage};

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
    /// Reasoning effort level for reasoning models (Off = omitted from requests).
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

    /// Calibrated chars-per-token ratio (×100). Falls back to the static
    /// default until the server has reported real usage.
    pub fn chars_per_token_x100(&self) -> u32 {
        self.chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Estimate the token count of `chars` of content using the calibrated
    /// ratio. Clamped to `chars` (a token is never shorter than 1 char, so
    /// this is a guaranteed over-estimate and never an under-estimate).
    pub fn estimate_tokens_from_chars(&self, chars: usize) -> usize {
        let c = self
            .chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(100) as usize;
        chars.saturating_mul(100) / c
    }

    /// Char budget for a given percentage of the current n_ctx window,
    /// converted from tokens to chars via the calibrated chars-per-token
    /// ratio. Returns 0 when the window size is unknown.
    fn char_budget_pct(&self, pct: u64) -> usize {
        let n_ctx = self.n_ctx();
        if n_ctx == 0 {
            return 0;
        }
        // n_ctx tokens × (pct / 100) × (c / 100) chars/token
        let c = self
            .chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(100) as u64;
        ((n_ctx as u64) * pct * c / 10_000) as usize
    }

    /// Trim trigger in char units for the current n_ctx: 90% of the window,
    /// converted via the calibrated ratio. Trimming only kicks in once the
    /// estimated conversation exceeds this.
    pub fn trim_trigger_chars(&self) -> usize {
        self.char_budget_pct(Self::TRIM_TRIGGER_PCT)
    }

    /// Trim target in char units for the current n_ctx: 50% of the window,
    /// converted via the calibrated ratio. Once the trigger is exceeded, the
    /// conversation is trimmed all the way down to this — far below the
    /// limit, not just under it.
    pub fn trim_target_chars(&self) -> usize {
        self.char_budget_pct(Self::TRIM_TARGET_PCT)
    }

    /// Record the estimator char count of a prompt about to be sent, so the
    /// next [`Self::calibrate_from_usage`] can compare it against the
    /// server-reported `prompt_tokens`.
    pub fn note_prompt_chars(&self, chars: usize) {
        *self.last_prompt_chars.lock().unwrap() = chars;
    }

    /// Update the chars-per-token calibration from a server-reported
    /// `usage.prompt_tokens`. Clamped to [1.0, 10.0] chars/token — anything
    /// outside that range indicates a measurement glitch (e.g. a server that
    /// counts only a subset of messages) and is ignored.
    pub fn calibrate_from_usage(&self, usage: Option<&Usage>) {
        let Some(usage) = usage else {
            return;
        };
        let prompt_tokens = usage.prompt_tokens;
        if prompt_tokens == 0 {
            return;
        }
        let chars = *self.last_prompt_chars.lock().unwrap();
        if chars == 0 {
            return;
        }
        // ratio × 100 = chars / prompt_tokens × 100
        let ratio_x100 = (chars as u64) * 100 / prompt_tokens as u64;
        let clamped = ratio_x100.clamp(100, 1000);
        self.chars_per_token_x100
            .store(clamped as u32, std::sync::atomic::Ordering::Relaxed);
        tracing::debug!(
            "calibrated chars/token to {:.2} (chars={}, prompt_tokens={})",
            clamped as f32 / 100.0,
            chars,
            prompt_tokens
        );
    }

    /// Measure the true chars/token ratio from a just-failed overflow request
    /// (the prompt we noted via [`Self::note_prompt_chars`] vs. the server's
    /// reported `n_prompt_tokens`), update the calibration, and return the
    /// **char** budget to trim the retry down to: 85% of the reported window.
    /// Falls back to the current calibration when the measurement is unusable.
    pub fn overflow_retry_char_budget(&self, ov: &ContextOverflow) -> usize {
        let chars = *self.last_prompt_chars.lock().unwrap();
        if ov.n_prompt > 0 && chars > 0 {
            let ratio_x100 = ((chars as u64) * 100 / ov.n_prompt as u64).clamp(100, 1000);
            self.chars_per_token_x100
                .store(ratio_x100 as u32, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                "overflow measured chars/token = {:.2} (chars={}, n_prompt={})",
                ratio_x100 as f32 / 100.0,
                chars,
                ov.n_prompt
            );
        }
        let ratio_x100 = self
            .chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(100) as u64;
        ((ov.n_ctx as u64) * 85 * ratio_x100) as usize / 10_000
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

    pub fn trim_conversation(&self, max_messages: usize) {
        session::trim_conversation(&self.conversation, max_messages);
    }

    /// Trim the client's conversation to the given token budget.
    /// Returns the number of messages removed.
    pub fn trim_to_token_budget(&self, target_tokens: usize) -> usize {
        let trimming = ContextTrimming::new();
        let config = crate::trimming::TrimConfig::default();
        trimming.trim_conversation(&self.conversation, target_tokens, &config)
    }

    /// Trim a standalone message vec to the given token budget.
    /// Used by the agent loop to trim its own history, since streaming
    /// writes to a throwaway conversation and never updates this field.
    pub fn trim_to_token_budget_messages(
        messages: &mut Vec<Message>,
        target_tokens: usize,
    ) -> usize {
        let trimming = ContextTrimming::new();
        let config = crate::trimming::TrimConfig::default();
        trimming.trim_messages(messages, target_tokens, &config)
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

    pub fn load_session(&mut self) -> Option<crate::sessions::Session> {
        session::load_session(
            self.session_id.as_deref(),
            &self.session_dir,
            &self.conversation,
            &mut self.system_prompt,
            self.encryption_key.as_ref(),
        )
    }

    pub fn save_session(&self) -> Result<(), anyhow::Error> {
        save_session(
            self.session_id.as_deref(),
            &self.session_dir,
            &self.conversation,
            &self.system_prompt,
            self.encryption_key.as_ref(),
            &self.save_queue,
            &self.save_failed,
        )
    }

    /// Enqueue a pending save and set the failure flag for UI notification.
    pub fn enqueue_save_failure(&self, error: &anyhow::Error) {
        session::enqueue_save_failure(&self.save_queue, &self.save_failed, error);
    }

    /// Try to retry any pending saves and clear the queue on success.
    pub fn retry_pending_saves(&self) {
        let _ = session::retry_pending_saves(&self.save_queue, &self.save_failed, &|| {
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

    /// Returns true if there is a pending save failure notification to show.
    pub fn has_save_failure(&self) -> bool {
        session::has_save_failure(&self.save_failed)
    }

    /// Clear the save failure flag (call after a successful save or user dismissal).
    pub fn clear_save_failure(&self) {
        session::clear_save_failure(&self.save_failed);
    }

    // ── HTTP methods ──────────────────────────────────────────────────────────

    pub async fn send_message(&self, prompt: &str) -> Result<(String, Option<Usage>), Error> {
        self.send_message_with_tools(prompt, None).await
    }

    /// Send a non-streaming request built from an explicit message list.
    ///
    /// Unlike `send_message` (which prepends `self.system_prompt` and appends
    /// the prompt to `self.conversation`), this uses the given messages
    /// verbatim — the caller is responsible for including the system prompt,
    /// history, and user turn in the right order.
    pub async fn complete_messages(
        &self,
        messages: &[Message],
        tools: Option<&[crate::tools::ToolDefinition]>,
    ) -> Result<(String, Option<Usage>), Error> {
        let mut msgs = messages.to_vec();
        let request = ChatRequest {
            model: "local".to_string(),
            messages: msgs.clone(),
            stream: false,
            tools: tools.map(|t| t.to_vec()),
            reasoning_effort: self.reasoning_effort.as_wire_value().map(|s| s.to_string()),
            stream_options: Some(http::StreamOptions {
                include_usage: true,
            }),
            return_progress: None,
        };
        self.note_prompt_chars(message_char_count(&msgs));
        let result = send_message(
            &self.http_client,
            &self.url(),
            self.settings.api_key().as_deref(),
            &request,
        )
        .await;

        // Backstop: the estimator is a heuristic — if the server still
        // rejects the request as over-context, force-trim the message list
        // to 80% of the reported prompt size and retry once.
        let result = match result {
            Err(e) if parse_context_overflow(&e).is_some() => {
                if let Some(ov) = parse_context_overflow(&e) {
                    tracing::warn!(
                        "complete_messages exceeded context ({} tokens, n_ctx={}); force-trimming and retrying",
                        ov.n_prompt, ov.n_ctx
                    );
                    let target = self.overflow_retry_char_budget(&ov);
                    Self::trim_to_token_budget_messages(&mut msgs, target);
                    let request2 = ChatRequest {
                        model: "local".to_string(),
                        messages: msgs.clone(),
                        stream: false,
                        tools: tools.map(|t| t.to_vec()),
                        reasoning_effort: self
                            .reasoning_effort
                            .as_wire_value()
                            .map(|s| s.to_string()),
                        stream_options: Some(http::StreamOptions {
                            include_usage: true,
                        }),
                        return_progress: None,
                    };
                    self.note_prompt_chars(message_char_count(&msgs));
                    match send_message(
                        &self.http_client,
                        &self.url(),
                        self.settings.api_key().as_deref(),
                        &request2,
                    )
                    .await
                    {
                        // Full result flows to the common tail below, where
                        // the usage is logged and the ratio calibrated.
                        Ok(ok) => {
                            self.calibrate_from_usage(ok.usage.as_ref());
                            Ok(ok)
                        }
                        Err(re) => Err(re),
                    }
                } else {
                    return Err(e);
                }
            }
            other => other,
        };

        let r = result?;
        self.record_usage(
            r.usage.as_ref(),
            r.model.as_deref(),
            r.tool_calls,
            r.thinking_chars,
        );
        self.calibrate_from_usage(r.usage.as_ref());
        Ok((r.content, r.usage))
    }

    pub async fn send_message_with_tools(
        &self,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
    ) -> Result<(String, Option<Usage>), Error> {
        let request = build_request(
            &self.system_prompt,
            &self.conversation,
            prompt,
            false,
            tools,
            self.reasoning_effort,
            self.n_ctx(),
        );
        self.note_prompt_chars(message_char_count(&request.messages));
        let result = send_message(
            &self.http_client,
            &self.url(),
            self.settings.api_key().as_deref(),
            &request,
        )
        .await;

        // Backstop: the estimator is a heuristic — if the server still
        // rejects the request as over-context, force-trim to 80% of the
        // reported prompt size and retry once.
        let result = match result {
            Err(e) if parse_context_overflow(&e).is_some() => {
                if let Some(ov) = parse_context_overflow(&e) {
                    tracing::warn!(
                        "request exceeded context ({} tokens, n_ctx={}); force-trimming and retrying",
                        ov.n_prompt, ov.n_ctx
                    );
                    let target = self.overflow_retry_char_budget(&ov);
                    self.trim_conversation(self.max_messages);
                    self.trim_to_token_budget(target);
                    let request2 = build_request(
                        &self.system_prompt,
                        &self.conversation,
                        prompt,
                        false,
                        tools,
                        self.reasoning_effort,
                        self.n_ctx(),
                    );
                    self.note_prompt_chars(message_char_count(&request2.messages));
                    let retry = send_message(
                        &self.http_client,
                        &self.url(),
                        self.settings.api_key().as_deref(),
                        &request2,
                    )
                    .await;
                    match retry {
                        // Full result flows to the common tail below, where
                        // the usage is logged and the ratio calibrated.
                        Ok(ok) => {
                            self.calibrate_from_usage(ok.usage.as_ref());
                            Ok(ok)
                        }
                        Err(re) => Err(re),
                    }
                } else {
                    return Err(e);
                }
            }
            other => other,
        };
        let r = result?;
        self.record_usage(
            r.usage.as_ref(),
            r.model.as_deref(),
            r.tool_calls,
            r.thinking_chars,
        );
        let (content, usage) = (r.content, r.usage);

        // Update conversation history
        let mut conv = self.conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
        drop(conv);

        // Calibrate the chars/token ratio from the server's real count so
        // subsequent trim budgets track the actual tokenizer.
        self.calibrate_from_usage(usage.as_ref());

        // Only trim once the context limit is reached (same policy as the
        // streaming chat loop): while the estimated token count stays below
        // 90% of n_ctx, the full history is kept.
        let n_ctx = self.n_ctx();
        if n_ctx > 0 {
            if estimate_conversation_tokens(&self.conversation) > self.trim_trigger_chars() {
                let target_chars = self.trim_target_chars();
                self.trim_conversation(self.max_messages);
                self.trim_to_token_budget(target_chars);
            }
        } else if self.max_messages > 0 {
            self.trim_conversation(self.max_messages);
        }

        Ok((content, usage))
    }

    /// Stream a request built from an EXPLICIT message list, without touching
    /// the client's own conversation. The agent engine keeps its own message
    /// history and uses this to retain full control (system prompt, assistant
    /// tool-call messages, tool results, reasoning round-trip).
    ///
    /// Thinking/reasoning chunks are delivered via `callback` with
    /// `is_thinking == true`; content chunks with `false`.
    ///
    /// Returns the accumulated assistant message (content, reasoning_content,
    /// tool_calls) plus the usage reported by the server.
    ///
    /// `on_tool_call_ready` fires (mid-stream) as soon as a tool call is
    /// complete enough to execute — the model has moved past it (text or the
    /// next tool call) and its arguments look like complete JSON. Agents use
    /// this to start executing tools while the model keeps reasoning.
    ///
    /// `on_prompt_progress` fires per server tick with llama.cpp's live
    /// prompt-processing progress (the request sets `return_progress: true`;
    /// other backends simply never invoke it).
    pub async fn stream_with_messages_arc(
        client: &Arc<Self>,
        messages: &[Message],
        tools: Option<&[crate::tools::ToolDefinition]>,
        callback: impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static,
        on_tool_call_ready: impl FnMut(ToolCall) + Send + Sync + 'static,
        on_prompt_progress: impl FnMut(crate::types::PromptProgress) + Send + Sync + 'static,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<(Message, Option<Usage>), Error> {
        let http_client = client.stream_http_client.clone();
        let base_url = client.url();
        let api_key = client.settings.api_key();

        let request = ChatRequest {
            model: "local".to_string(),
            messages: messages.to_vec(),
            stream: true,
            tools: tools.map(|t| t.to_vec()),
            reasoning_effort: client
                .reasoning_effort
                .as_wire_value()
                .map(|s| s.to_string()),
            stream_options: Some(http::StreamOptions {
                include_usage: true,
            }),
            // Ask llama.cpp for live prompt-processing progress chunks.
            return_progress: Some(true),
        };
        let body = serde_json::to_string(&request)?;

        let mut builder = http_client
            .post(format!("{}/v1/chat/completions", base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body);
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let resp = builder.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Http(format!("Server returned {}: {}", status, text)));
        }

        // Throwaway conversation seeded with one empty assistant message; the
        // SSE layer accumulates content / reasoning_content / tool_calls into it.
        let local_conv: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(vec![Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }]));

        let mut boxed_cb = Box::new(callback);
        let mut boxed_ready = Box::new(on_tool_call_ready);
        let mut boxed_pp = Box::new(on_prompt_progress);
        let mut ready_tracker = ToolCallTracker::default();
        let (usage, model) = sse::stream_message(
            resp,
            &local_conv,
            &mut boxed_cb,
            &mut boxed_ready,
            &mut boxed_pp,
            &mut ready_tracker,
            cancel_token,
        )
        .await?;

        let msg = local_conv.lock().unwrap().pop().unwrap_or_else(|| Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });

        // The accumulated message carries what the SSE layer saw: the
        // assistant's tool calls and its thinking text.
        let tool_calls = msg.tool_calls.as_ref().map(|t| t.len() as u32).unwrap_or(0);
        let thinking_chars = msg
            .reasoning_content
            .as_ref()
            .map(|r| r.chars().count() as u64)
            .unwrap_or(0);
        client.record_usage(usage.as_ref(), model.as_deref(), tool_calls, thinking_chars);
        Ok((msg, usage))
    }

    // ── Tool call helpers ─────────────────────────────────────────────────────

    /// Check for malformed tool calls in the conversation and return warnings.
    /// A tool call is considered malformed if its arguments are not valid JSON.
    pub fn check_tool_call_warnings(&self) -> Vec<(String, String)> {
        let conv = self.conversation.lock().unwrap();
        let mut warnings = Vec::new();

        for msg in conv.iter() {
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    // Try to parse the arguments as JSON
                    if tc.function.arguments.is_empty() {
                        warnings.push((tc.function.name.clone(), "Empty arguments".to_string()));
                    } else if !tc.function.arguments.starts_with('{') {
                        warnings.push((
                            tc.function.name.clone(),
                            "Invalid JSON: arguments don't start with '{'".to_string(),
                        ));
                    } else if serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        .is_err()
                    {
                        warnings.push((
                            tc.function.name.clone(),
                            format!(
                                "Malformed JSON arguments: {}",
                                &tc.function.arguments[..tc.function.arguments.len().min(50)]
                            ),
                        ));
                    }
                }
            }
        }

        warnings
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
