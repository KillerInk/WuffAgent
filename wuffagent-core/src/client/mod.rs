use std::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub mod http;
pub mod sse;
pub mod session;
pub mod pipeline;

pub use pipeline::ChatPipeline;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::types::{Message, Usage};

// Re-export key types so the public API surface is unchanged
pub use http::{build_request, send_message, build_stream_request, ChatRequest, Response, Choice};
pub use sse::{process_sse_line, stream_message, add_streaming_messages};
pub use session::{
    save_session, load_session,
    enqueue_save_failure, retry_pending_saves, has_save_failure,
    clear_save_failure,
    trim_conversation, clear_history, clear_session_messages,
};
// Re-export trimming helpers for backward compatibility
pub use crate::trimming::{estimate_tokens, message_char_count, ContextTrimming};

/// Backward-compat wrapper: delegates to `ContextTrimming::trim_to_token_budget`.
pub fn trim_to_token_budget(conversation: &std::sync::Arc<std::sync::Mutex<Vec<crate::types::Message>>>, target_tokens: usize) -> usize {
    let trimming = ContextTrimming::new();
    let config = crate::trimming::TrimConfig::default();
    trimming.trim_to_token_budget(&mut conversation.lock().unwrap(), target_tokens, &config)
}

/// Backward-compat wrapper: delegates to `ContextTrimming::trim_messages`.
pub fn trim_to_token_budget_messages(messages: &mut Vec<crate::types::Message>, target_tokens: usize) -> usize {
    let trimming = ContextTrimming::new();
    let config = crate::trimming::TrimConfig::default();
    trimming.trim_messages(messages, target_tokens, &config)
}

/// Backward-compat wrapper: estimates conversation tokens via `message_char_count`.
pub fn estimate_conversation_tokens(conversation: &std::sync::Arc<std::sync::Mutex<Vec<crate::types::Message>>>) -> usize {
    message_char_count(&conversation.lock().unwrap())
}

/// Details from a server `exceed_context_size_error` (HTTP 400): the prompt
/// size that was sent and the server's actual context window. Lets the caller
/// derive the true chars/token ratio from the failing request and force-trim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextOverflow {
    pub n_prompt: u32,
    pub n_ctx: u32,
}

/// Parse a server `exceed_context_size_error` (HTTP 400) into the reported
/// prompt size and context window, so the caller can force-trim to fit and
/// retry. Returns `None` for any other error.
pub fn parse_context_overflow(err: &Error) -> Option<ContextOverflow> {
    let Error::Http(msg) = err else {
        return None;
    };
    // Error format: "Server returned 400 Bad Request: {json body}[trailing text]"
    let body_start = msg.find('{')?;
    let body_end = msg.rfind('}')?;
    if body_end <= body_start {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(&msg[body_start..=body_end]).ok()?;
    let e = v.get("error")?;
    if e.get("type").and_then(|t| t.as_str()) != Some("exceed_context_size_error") {
        return None;
    }
    let n_prompt = e.get("n_prompt_tokens")?.as_u64()? as u32;
    let n_ctx = e.get("n_ctx").and_then(|t| t.as_u64()).unwrap_or(n_prompt as u64) as u32;
    Some(ContextOverflow { n_prompt, n_ctx })
}

/// Extract the server's `n_ctx` from a llama.cpp `/props` response body.
///
/// Servers report it under `default_generation_settings.n_ctx`; some builds or
/// proxies also expose it at the top level. The nested path wins, then
/// top-level. Returns `None` when the body is not JSON or neither field is a
/// number — callers treat that as "limit unknown" (trimming disabled) rather
/// than guessing a fallback.
pub fn parse_props_n_ctx(body: &str) -> Option<u32> {
    let props: serde_json::Value = serde_json::from_str(body).ok()?;
    props
        .get("default_generation_settings")
        .and_then(|s| s.get("n_ctx"))
        .or_else(|| props.get("n_ctx"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
}

#[derive(Clone)]
pub struct ChatClient {
    base_url: String,
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
    api_key: Option<String>,
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
}

/// Strip think tags and their contents from model output (for display).
/// The content between the tags is kept, prefixed with a thinking marker.
/// Tag literals are assembled via `concat!` so the raw sequence is not
/// spelled out in source.
pub fn strip_think_tags(text: &str) -> String {
    const OPEN: &str = concat!("<", "think>");
    const CLOSE: &str = concat!("<", "/think>");
    let mut out = String::new();
    let mut rest = text;
    loop {
        match rest.find(OPEN) {
            Some(o) => {
                out.push_str(&rest[..o]);
                let after_open = &rest[o + OPEN.len()..];
                match after_open.find(CLOSE) {
                    Some(c) => {
                        let inner = &after_open[..c];
                        rest = &after_open[c + CLOSE.len()..];
                        let trimmed = inner.trim();
                        if !trimmed.is_empty() {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str("💭 ");
                            out.push_str(trimmed);
                            out.push('\n');
                        }
                    }
                    None => {
                        // Unclosed tag: keep the remainder as-is
                        out.push_str(&rest[o..]);
                        break;
                    }
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
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
            base_url: base_url.to_string(),
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
            api_key: None,
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
        }
    }

    pub fn set_tool_event_sender(&self, tx: mpsc::Sender<crate::types::AppEvent>) {
        *self.tool_event_tx.lock().unwrap() = Some(tx);
    }

    pub fn set_max_messages(&mut self, max_messages: usize) {
        self.max_messages = max_messages;
    }

    /// Update the effective n_ctx in place. Takes `&self` (interior mutability)
    /// so callers can sync the server's reported context size on a shared client
    /// without cloning it — every clone sees the new value immediately.
    pub fn set_n_ctx(&self, n_ctx: u32) {
        self.n_ctx.store(n_ctx, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn n_ctx(&self) -> u32 {
        self.n_ctx.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Calibrated chars-per-token ratio (×100). Falls back to the static
    /// default until the server has reported real usage.
    pub fn chars_per_token_x100(&self) -> u32 {
        self.chars_per_token_x100.load(std::sync::atomic::Ordering::Relaxed)
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
            let ratio_x100 =
                ((chars as u64) * 100 / ov.n_prompt as u64).clamp(100, 1000);
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

    pub fn set_url(&mut self, url: &str) {
        self.base_url = url.to_string();
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn set_api_key(&mut self, key: Option<&str>) {
        self.api_key = key.map(|s| s.to_string());
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
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
        session::clear_session_messages(
            &self.conversation,
            &|| save_session(
                self.session_id.as_deref(),
                &self.session_dir,
                &self.conversation,
                &self.system_prompt,
                self.encryption_key.as_ref(),
                &self.save_queue,
                &self.save_failed,
            ),
        );
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
    pub fn trim_to_token_budget_messages(messages: &mut Vec<Message>, target_tokens: usize) -> usize {
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
        let _ = session::retry_pending_saves(
            &self.save_queue,
            &self.save_failed,
            &|| save_session(
                self.session_id.as_deref(),
                &self.session_dir,
                &self.conversation,
                &self.system_prompt,
                self.encryption_key.as_ref(),
                &self.save_queue,
                &self.save_failed,
            ),
        );
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

    pub async fn send_message(
        &self,
        prompt: &str,
    ) -> Result<(String, Option<Usage>), Error> {
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
            stream_options: Some(http::StreamOptions { include_usage: true }),
        };
        self.note_prompt_chars(message_char_count(&msgs));
        let result = send_message(
            &self.http_client,
            &self.base_url,
            self.api_key.as_deref(),
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
                        reasoning_effort: self.reasoning_effort.as_wire_value().map(|s| s.to_string()),
                        stream_options: Some(http::StreamOptions { include_usage: true }),
                    };
                    self.note_prompt_chars(message_char_count(&msgs));
                    match send_message(
                        &self.http_client,
                        &self.base_url,
                        self.api_key.as_deref(),
                        &request2,
                    )
                    .await
                    {
                        Ok(ok) => {
                            self.calibrate_from_usage(ok.1.as_ref());
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

        let (content, usage) = result?;
        self.calibrate_from_usage(usage.as_ref());
        Ok((content, usage))
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
            &self.base_url,
            self.api_key.as_deref(),
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
                        &self.base_url,
                        self.api_key.as_deref(),
                        &request2,
                    )
                    .await;
                    match retry {
                        Ok(ok) => {
                            self.calibrate_from_usage(ok.1.as_ref());
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
        let (content, usage) = result?;

        // Update conversation history
        let mut conv = self.conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
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
    pub async fn stream_with_messages_arc(
        client: &Arc<Self>,
        messages: &[Message],
        tools: Option<&[crate::tools::ToolDefinition]>,
        callback: impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<(Message, Option<Usage>), Error> {
        let http_client = client.stream_http_client.clone();
        let base_url = client.base_url.clone();
        let api_key = client.api_key.clone();

        let request = ChatRequest {
            model: "local".to_string(),
            messages: messages.to_vec(),
            stream: true,
            tools: tools.map(|t| t.to_vec()),
            reasoning_effort: client.reasoning_effort.as_wire_value().map(|s| s.to_string()),
            stream_options: Some(http::StreamOptions { include_usage: true }),
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
            return Err(Error::Http(format!(
                "Server returned {}: {}",
                status, text
            )));
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
        }]));

        let mut boxed_cb = Box::new(callback);
        let usage = sse::stream_message(resp, &local_conv, &mut boxed_cb, cancel_token).await?;

        let msg = local_conv.lock().unwrap().pop().unwrap_or_else(|| Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

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
                        warnings.push((
                            tc.function.name.clone(),
                            "Empty arguments".to_string(),
                        ));
                    } else if !tc.function.arguments.starts_with('{') {
                        warnings.push((
                            tc.function.name.clone(),
                            "Invalid JSON: arguments don't start with '{'".to_string(),
                        ));
                    } else if serde_json::from_str::<serde_json::Value>(&tc.function.arguments).is_err() {
                        warnings.push((
                            tc.function.name.clone(),
                            format!("Malformed JSON arguments: {}", &tc.function.arguments[..tc.function.arguments.len().min(50)]),
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
mod tests {
    use super::*;

    #[test]
    fn test_build_request_reasoning_effort() {
        let mut client = ChatClient::new("http://localhost:8080");

        // Off: field omitted from JSON entirely
        client.set_reasoning_effort(crate::types::ReasoningEffort::Off);
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            None,
            client.reasoning_effort(),
            4096,
        );
        assert!(request.reasoning_effort.is_none());
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("reasoning_effort"));

        // High: serialized as "xhigh" (Qwen3 template wire value)
        client.set_reasoning_effort(crate::types::ReasoningEffort::High);
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            None,
            client.reasoning_effort(),
            4096,
        );
        assert_eq!(request.reasoning_effort.as_deref(), Some("xhigh"));
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""reasoning_effort":"xhigh""#));
    }

    #[test]
    fn test_build_request_no_system_prompt() {
        let client = ChatClient::new("http://localhost:8080");
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            None,
            crate::types::ReasoningEffort::default(),
            4096,
        );
        assert_eq!(request.model, "local");
        assert!(!request.stream);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hello");
    }

    #[test]
    fn test_calibrate_from_usage() {
        let client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.chars_per_token_x100(), 350); // default 3.5

        // 10000 chars -> 3000 tokens => ratio 3.333
        client.note_prompt_chars(10_000);
        client.calibrate_from_usage(Some(&crate::types::Usage {
            prompt_tokens: 3000,
            completion_tokens: 100,
            total_tokens: 3100,
        }));
        assert_eq!(client.chars_per_token_x100(), 333);

        // CJK content: 500 chars -> 500 tokens => ratio 1.0 (clamped lower bound)
        client.note_prompt_chars(500);
        client.calibrate_from_usage(Some(&crate::types::Usage {
            prompt_tokens: 500,
            completion_tokens: 10,
            total_tokens: 510,
        }));
        assert_eq!(client.chars_per_token_x100(), 100);

        // No usage / zero tokens: ratio unchanged
        client.note_prompt_chars(1000);
        client.calibrate_from_usage(None);
        client.calibrate_from_usage(Some(&crate::types::Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        }));
        assert_eq!(client.chars_per_token_x100(), 100);

        // Glitched measurement (ratio 100.0) clamps to the 10.0 upper bound
        client.note_prompt_chars(1000);
        client.calibrate_from_usage(Some(&crate::types::Usage {
            prompt_tokens: 1,
            completion_tokens: 0,
            total_tokens: 1,
        }));
        assert_eq!(client.chars_per_token_x100(), 1000);
    }

    #[test]
    fn test_trim_trigger_and_target_chars_use_calibrated_ratio() {
        let client = ChatClient::new("http://localhost:8080");
        client.set_n_ctx(100_096);
        // Default 3.5 chars/token:
        // trigger: 100096 × 0.9 × 3.5 = 315292.8 → 315292
        assert_eq!(client.trim_trigger_chars(), (100_096u64 * 90 * 350 / 10_000) as usize);
        // target:  100096 × 0.5 × 3.5 = 175168.0 → 175168
        assert_eq!(client.trim_target_chars(), (100_096u64 * 50 * 350 / 10_000) as usize);

        // Calibrate to 3.0 chars/token (English prose): both budgets shrink
        client.note_prompt_chars(30_000);
        client.calibrate_from_usage(Some(&crate::types::Usage {
            prompt_tokens: 10_000,
            completion_tokens: 0,
            total_tokens: 10_000,
        }));
        assert_eq!(client.trim_trigger_chars(), (100_096u64 * 90 * 300 / 10_000) as usize);
        assert_eq!(client.trim_target_chars(), (100_096u64 * 50 * 300 / 10_000) as usize);

        // n_ctx = 0 → no budget
        client.set_n_ctx(0);
        assert_eq!(client.trim_trigger_chars(), 0);
        assert_eq!(client.trim_target_chars(), 0);
    }

    #[test]
    fn test_estimate_tokens_from_chars_over_estimates() {
        let client = ChatClient::new("http://localhost:8080");
        // Default 3.5: 7000 chars => 2000 tokens
        assert_eq!(client.estimate_tokens_from_chars(7000), 2000);
        // Never an under-estimate: at ratio 1.0 tokens == chars
        client.note_prompt_chars(500);
        client.calibrate_from_usage(Some(&crate::types::Usage {
            prompt_tokens: 500,
            completion_tokens: 0,
            total_tokens: 500,
        }));
        assert_eq!(client.estimate_tokens_from_chars(1234), 1234);
    }

    #[test]
    fn test_parse_context_overflow() {
        let body = r#"{"error":{"code":400,"message":"request (104102 tokens) exceeds the available context size (100096 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":104102,"n_ctx":100096}}"#;
        let err = Error::Http(format!("Server returned 400 Bad Request: {}", body));
        let ov = parse_context_overflow(&err).expect("should parse overflow");
        assert_eq!(ov.n_prompt, 104_102);
        assert_eq!(ov.n_ctx, 100_096);

        // Other 400 errors must not trigger a force-trim retry.
        let err = Error::Http(
            "Server returned 400 Bad Request: {\"error\":{\"message\":\"bad request\"}}"
                .to_string(),
        );
        assert_eq!(parse_context_overflow(&err), None);

        // Non-HTTP errors never match.
        assert_eq!(parse_context_overflow(&Error::Cancelled), None);
        assert_eq!(parse_context_overflow(&Error::Stream("boom".into())), None);
    }

    #[test]
    fn test_parse_props_n_ctx_nested() {
        let body = r#"{"default_generation_settings":{"n_ctx":8192,"temp":0.8}}"#;
        assert_eq!(parse_props_n_ctx(body), Some(8192));
    }

    #[test]
    fn test_parse_props_n_ctx_top_level_fallback() {
        let body = r#"{"n_ctx":16384,"models":[]}"#;
        assert_eq!(parse_props_n_ctx(body), Some(16384));
    }

    #[test]
    fn test_parse_props_n_ctx_nested_wins() {
        let body = r#"{"n_ctx":4096,"default_generation_settings":{"n_ctx":32768}}"#;
        assert_eq!(parse_props_n_ctx(body), Some(32768));
    }

    #[test]
    fn test_parse_props_n_ctx_absent_or_invalid() {
        assert_eq!(parse_props_n_ctx(r#"{"models":[]}"#), None);
        assert_eq!(parse_props_n_ctx(r#"{"default_generation_settings":{}}"#), None);
        assert_eq!(parse_props_n_ctx("not json"), None);
        assert_eq!(parse_props_n_ctx(""), None);
    }

    #[test]
    fn test_build_request_with_system_prompt() {
        let mut client = ChatClient::new("http://localhost:8080");
        client.set_system_prompt("You are helpful.");
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            None,
            crate::types::ReasoningEffort::default(),
            4096,
        );
        assert_eq!(request.model, "local");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[0].content, "You are helpful.");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "Hello");
    }

    #[test]
    fn test_build_request_streaming() {
        let client = ChatClient::new("http://localhost:8080");
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            true,
            None,
            crate::types::ReasoningEffort::default(),
            4096,
        );
        assert!(request.stream);
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_build_request_with_tools() {
        let client = ChatClient::new("http://localhost:8080");
        let tools = vec![
            crate::tools::ToolDefinition {
                type_name: "function".to_string(),
                function: crate::tools::ToolFunctionSpec {
                    name: "test_tool".to_string(),
                    description: "A test tool".to_string(),
                    parameters: crate::tools::JsonSchema {
                        type_name: "object".to_string(),
                        properties: None,
                        required: vec![],
                    },
                },
            }
        ];
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            Some(&tools),
            crate::types::ReasoningEffort::default(),
            4096,
        );
        assert!(request.tools.is_some());
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_build_request_includes_history() {
        let client = ChatClient::new("http://localhost:8080");
        {
            let mut conv = client.conversation.lock().unwrap();
            conv.push(Message { role: "user".into(), content: "Hi there".into(), timestamp: String::new(), tool_calls: None, tool_call_id: None, reasoning_content: None });
                        conv.push(Message { role: "assistant".into(), content: "Hello! How can I help?".into(), timestamp: String::new(), tool_calls: None, tool_call_id: None, reasoning_content: None });
        }
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "What's the weather?",
            false,
            None,
            crate::types::ReasoningEffort::default(),
            4096,
        );
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hi there");
        assert_eq!(request.messages[1].role, "assistant");
        assert_eq!(request.messages[1].content, "Hello! How can I help?");
        assert_eq!(request.messages[2].role, "user");
        assert_eq!(request.messages[2].content, "What's the weather?");
    }

    #[test]
    fn test_build_request_skips_empty_assistant_message() {
        let client = ChatClient::new("http://localhost:8080");
        {
            let mut conv = client.conversation.lock().unwrap();
            conv.push(Message { role: "user".into(), content: "Hi".into(), timestamp: String::new(), tool_calls: None, tool_call_id: None, reasoning_content: None });
                        conv.push(Message { role: "assistant".into(), content: String::new(), timestamp: String::new(), tool_calls: None, tool_call_id: None, reasoning_content: None });
        }
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Follow up",
            false,
            None,
            crate::types::ReasoningEffort::default(),
            4096,
        );
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hi");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "Follow up");
    }

    #[test]
    fn test_chat_client_api_key() {
        let mut client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.api_key(), None);
        client.set_api_key(Some("sk-test"));
        assert_eq!(client.api_key(), Some("sk-test"));
    }

    #[test]
    fn test_chat_client_base_url() {
        let client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[tokio::test]
    async fn test_process_sse_line_empty() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_sse_line_done() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("data: [DONE]", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_done_trailing_newline() {
        // stream_message slices lines up to and including '\n', so the
        // stream-end frame arrives as "data: [DONE]\n". Regression test:
        // before the fix this fell through to JSON parsing and logged
        // "SSE: skipping unparseable data line".
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("data: [DONE]\n", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_done_crlf() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("data: [DONE]\r\n", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_data_prefix_without_space() {
        // Some servers emit "data:{...}" without the space after the colon.
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        drop(conv);

        let sse_data = r#"data:{"choices":[{"delta":{"content":"Hi"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line(sse_data, &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert_eq!(captured, vec!["Hi"]);
        let c = client.conversation().lock().unwrap();
        assert_eq!(c[0].content, "Hi");
    }

    #[tokio::test]
    async fn test_process_sse_line_non_data() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("id: 1", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_valid_chunk() {
        let client = ChatClient::new("http://localhost:8080");
        // Pre-seed an empty assistant message (caller does this in stream_message)
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        drop(conv);

        let sse_data = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line(sse_data, &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert_eq!(captured, vec!["Hello"]);
        let c = client.conversation().lock().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].role, "assistant");
        assert_eq!(c[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_process_sse_line_multiple_chunks() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        drop(conv);

        let sse1 = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        process_sse_line(sse1, &mut cb, &client.conversation()).await.unwrap();

        let sse2 = r#"data: {"choices":[{"delta":{"content":" world"}}]}"#;
        let mut captured2 = Vec::new();
        let mut cb2 = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured2.push(s);
            Ok(())
        };
        process_sse_line(sse2, &mut cb2, &client.conversation()).await.unwrap();

        assert_eq!(captured, vec!["Hello"]);
        assert_eq!(captured2, vec![" world"]);
        let c = client.conversation().lock().unwrap();
        assert_eq!(c[0].content, "Hello world");
    }

    #[tokio::test]
    async fn test_process_sse_line_empty_content() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        drop(conv);

        let sse_data = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line(sse_data, &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[test]
    fn test_check_tool_call_warnings_empty_args() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: "".to_string(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "tc1".to_string(),
                call_type: "function".to_string(),
                function: crate::types::ToolFunction {
                    name: "test_tool".to_string(),
                    arguments: "".to_string(),
                },
            }]),
            tool_call_id: None,
        reasoning_content: None,
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, "test_tool");
        assert_eq!(warnings[0].1, "Empty arguments");
    }

    #[test]
    fn test_check_tool_call_warnings_invalid_json() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: "".to_string(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "tc1".to_string(),
                call_type: "function".to_string(),
                function: crate::types::ToolFunction {
                    name: "test_tool".to_string(),
                    arguments: "not json".to_string(),
                },
            }]),
            tool_call_id: None,
        reasoning_content: None,
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, "test_tool");
        assert!(warnings[0].1.contains("Invalid JSON"));
    }

    #[test]
    fn test_check_tool_call_warnings_valid_json() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: "".to_string(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "tc1".to_string(),
                call_type: "function".to_string(),
                function: crate::types::ToolFunction {
                    name: "test_tool".to_string(),
                    arguments: "{\"key\": \"value\"}".to_string(),
                },
            }]),
            tool_call_id: None,
        reasoning_content: None,
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert!(warnings.is_empty());
    }
}
