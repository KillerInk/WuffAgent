use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::Error;
use crate::types::{Message, Usage};

/// HTTP request building and sending for ChatClient.

/// A structured error from an OpenAI-compatible LLM server: the standard
/// `{"error": {...}}` envelope of HTTP 4xx/5xx responses. Carries the HTTP
/// status code plus the parsed `message` / `type` fields so callers can
/// branch on the error TYPE (e.g. llama.cpp's `exceed_context_size_error`)
/// instead of string-matching a formatted message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmError {
    /// HTTP status code the error response carried (not part of the JSON body).
    pub code: u16,
    /// `error.message` from the body (empty when absent).
    pub message: String,
    /// `error.type` from the body, e.g. "exceed_context_size_error",
    /// "invalid_request_error", "server_error"; empty when absent.
    pub error_type: String,
    /// llama.cpp extension on `exceed_context_size_error`: the prompt size
    /// that was sent (tokens).
    pub n_prompt_tokens: Option<u32>,
    /// llama.cpp extension on `exceed_context_size_error`: the server's
    /// context window (tokens).
    pub n_ctx: Option<u32>,
}

impl LlmError {
    /// Parse the OAI error envelope from an HTTP error response body.
    ///
    /// `None` when the body is not JSON with an `error` object or string —
    /// the caller then falls back to the raw [`crate::client::Error::Http`]
    /// string. Both `{"error": {"message": …}}` and `{"error": "message"}`
    /// shapes are accepted.
    pub fn parse(code: u16, body: &str) -> Option<LlmError> {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        let e = v.get("error")?;
        if e.is_string() {
            return Some(LlmError {
                code,
                message: e.as_str().unwrap_or_default().to_string(),
                error_type: String::new(),
                n_prompt_tokens: None,
                n_ctx: None,
            });
        }
        if !e.is_object() {
            return None;
        }
        Some(LlmError {
            code,
            message: e
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
            error_type: e
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            n_prompt_tokens: e
                .get("n_prompt_tokens")
                .and_then(|t| t.as_u64())
                .map(|t| t as u32),
            n_ctx: e.get("n_ctx").and_then(|t| t.as_u64()).map(|t| t as u32),
        })
    }

    /// True for transient conditions worth a retry: 502/503/504 gateway
    /// errors (llama.cpp answers 503 "model not loaded" while a model is
    /// still loading). 4xx errors are never retried here — the
    /// `exceed_context_size_error` 400 has its own dedicated retry path.
    pub fn is_retryable(&self) -> bool {
        matches!(self.code, 502 | 503 | 504)
    }

    /// The context-overflow details when this is llama.cpp's
    /// `exceed_context_size_error` (HTTP 400) with a reported prompt size.
    pub fn context_overflow(&self) -> Option<crate::trimming::ContextOverflow> {
        if self.error_type != "exceed_context_size_error" {
            return None;
        }
        let n_prompt = self.n_prompt_tokens?;
        Some(crate::trimming::ContextOverflow {
            n_prompt,
            n_ctx: self.n_ctx.unwrap_or(n_prompt),
        })
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.error_type.is_empty() {
            write!(f, "Server returned {}: {}", self.code, self.message)
        } else {
            write!(
                f,
                "Server returned {} ({:?}): {}",
                self.code, self.error_type, self.message
            )
        }
    }
}

impl std::error::Error for LlmError {}

/// Max retries for transient LLM request failures (502/503/504 +
/// connection errors before the response headers).
pub const MAX_TRANSIENT_RETRIES: u32 = 3;

/// Build the client [`Error`] for a non-success HTTP response: the
/// structured [`LlmError`] when the body parses as the OAI error envelope,
/// otherwise the legacy raw `Error::Http` string (which the legacy overflow
/// parser still understands).
pub fn llm_error_or_http(code: u16, body: &str) -> Error {
    match LlmError::parse(code, body) {
        Some(llm) => Error::Llm(llm),
        None => Error::Http(format!(
            "Server returned {}: {}",
            reqwest::StatusCode::from_u16(code)
                .map(|s| s.to_string())
                .unwrap_or_else(|_| code.to_string()),
            body
        )),
    }
}

/// Send an LLM request, retrying TRANSIENT failures with exponential
/// backoff (250ms, 500ms, 1s): connection-level errors (refused/reset
/// before the response headers) and 502/503/504 responses. Any other result
/// — success, or a non-transient HTTP status (4xx, other 5xx) — is returned
/// immediately so the caller can inspect the response.
///
/// `make` builds a FRESH request each attempt (reqwest request builders are
/// single-use). `cancel` (when given) interrupts a backoff sleep with
/// `Error::Cancelled`.
pub async fn send_with_transient_retries<F, Fut>(
    mut make: F,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<reqwest::Response, reqwest::Error>>,
{
    let mut delay = std::time::Duration::from_millis(250);
    let mut last_err: Option<Error> = None;
    for attempt in 0..=MAX_TRANSIENT_RETRIES {
        match make().await {
            Ok(resp) => {
                let status = resp.status();
                if !matches!(status.as_u16(), 502 | 503 | 504) {
                    return Ok(resp);
                }
                // Drain the body so the connection can be pooled; keep the
                // parsed error for the exhausted-retries report.
                let body = resp.text().await.unwrap_or_default();
                last_err = Some(llm_error_or_http(status.as_u16(), &body));
                tracing::debug!("Transient {status}; retry {attempt}/{MAX_TRANSIENT_RETRIES}");
            }
            Err(e) if e.is_connect() => {
                last_err = Some(Error::Http(e.to_string()));
                tracing::debug!("Connection error ({e}); retry {attempt}/{MAX_TRANSIENT_RETRIES}");
            }
            other => return other.map_err(Error::from),
        }
        if attempt == MAX_TRANSIENT_RETRIES {
            break;
        }
        match cancel {
            Some(tok) => {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = tok.cancelled() => return Err(Error::Cancelled),
                }
            }
            None => tokio::time::sleep(delay).await,
        }
        delay *= 2;
    }
    Err(last_err
        .unwrap_or_else(|| Error::Http(format!("request failed after {MAX_TRANSIENT_RETRIES} retries"))))
}

#[derive(Serialize, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<crate::tools::ToolDefinition>>,
    /// Reasoning effort for reasoning models (omitted when Off; see
    /// [`reasoning_wire`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Qwen3-style chat-template kwargs. `enable_thinking` makes the
    /// thinking on/off state explicit: Qwen3.x defaults to thinking ON
    /// (and `reasoning_effort` has no off-level), so `Off` must disable it
    /// here. Ignored by backends whose templates lack the kwarg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<ChatTemplateKwargs>,
    /// Request options for streaming (e.g. include_usage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// llama.cpp extension: request live prompt-processing progress
    /// (`prompt_progress` chunks) in stream mode. Ignored by other backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_progress: Option<bool>,
    /// llama.cpp extension: the reasoning-format marker for reasoning
    /// models (e.g. "deepseek", "deepseek-legacy" for DeepSeek-family
    /// models) — tells the server how to split `reasoning_content` out.
    /// Omitted (None) = server default; `strip_think_tags` remains the
    /// fallback for servers that don't split reasoning out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_format: Option<String>,
    /// llama.cpp extension: cap on reasoning tokens
    /// (`reasoning_budget_tokens` on the wire; -1 = server default).
    /// Omitted (None) = server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_budget_tokens: Option<i32>,
}

/// Borrowed view of [`ChatRequest`]: the same wire fields in the same order,
/// but `messages` and `tools` are borrowed slices, so the request body can be
/// serialized without first deep-cloning the whole conversation history +
/// tool definitions (perf P2 — the agent loop rebuilt an owned request every
/// LLM round, cloning the entire history just to serialize it again). The
/// serde attributes mirror the owned struct, so `serde_json::to_string`
/// output is byte-identical (golden test: `test_chat_request_ref_matches_owned`).
#[derive(Serialize, Debug)]
pub struct ChatRequestRef<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [crate::tools::ToolDefinition]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'a str>,
    /// Copy-able, kept by value — same wire output as the owned struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<ChatTemplateKwargs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<&'a StreamOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_progress: Option<bool>,
    /// llama.cpp extension — see [`ChatRequest::reasoning_format`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_format: Option<String>,
    /// llama.cpp extension — see [`ChatRequest::reasoning_budget_tokens`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_budget_tokens: Option<i32>,
}

impl ChatRequestRef<'_> {
    /// Serialize to the request body (no prior deep clone of messages/tools).
    pub fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(self)?)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChatTemplateKwargs {
    /// Qwen3 chat-template switch: `true` = thinking mode on,
    /// `false` = off (also skips the thinking prefix).
    pub enable_thinking: bool,
}

#[derive(Serialize, Debug)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Wire fields for a reasoning-effort setting: the `reasoning_effort` value
/// plus the explicit Qwen3-style `enable_thinking` kwarg. Single source of
/// truth for every request-construction site — see
/// [`crate::types::ReasoningEffort`] for the level→wire mapping.
pub fn reasoning_wire(
    effort: crate::types::ReasoningEffort,
) -> (Option<String>, Option<ChatTemplateKwargs>) {
    (
        effort.as_wire_value().map(str::to_string),
        Some(ChatTemplateKwargs {
            enable_thinking: effort.enable_thinking(),
        }),
    )
}

/// The llama.cpp reasoning tuning fields as request-body values
/// (`reasoning_format` + `reasoning_budget_tokens`). `None` for each is
/// omitted from the body (server default) — see the struct fields on
/// [`ChatRequest`].
pub fn reasoning_budget_wire(
    format: Option<&str>,
    budget: Option<i32>,
) -> (Option<String>, Option<i32>) {
    (format.map(str::to_string), budget)
}

#[derive(Deserialize, Debug)]
pub struct Response {
    /// Model the server actually used (echoed by OpenAI-compatible APIs).
    /// Requests hardcode `"local"`, so this is the only real model name.
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    /// llama.cpp extension: per-stage speeds (sibling of `usage` on the
    /// wire). Folded into `usage.timings` by `send_message`.
    #[serde(default)]
    pub timings: Option<crate::types::LlamaTimings>,
}

/// Everything a completed non-streaming call knows about itself, for the
/// usage log and callers alike.
#[derive(Debug, Clone)]
pub struct NonStreamResult {
    /// The assistant's text content.
    pub content: String,
    /// Server-reported token counts (None when the backend omitted them).
    pub usage: Option<Usage>,
    /// Model name the server reported (for the usage log).
    pub model: Option<String>,
    /// Number of tool calls the assistant issued in this call.
    pub tool_calls: u32,
    /// Character count of the model's thinking/reasoning text.
    pub thinking_chars: u64,
}

#[derive(Deserialize, Debug)]
pub struct Choice {
    pub message: Message,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Build a ChatRequest from the given client state.
pub fn build_request(
    system_prompt: &str,
    conversation: &std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
    prompt: &str,
    stream: bool,
    tools: Option<&[crate::tools::ToolDefinition]>,
    reasoning_effort: crate::types::ReasoningEffort,
    #[allow(unused_variables)] n_ctx: u32,
    reasoning_format: Option<&str>,
    reasoning_budget_tokens: Option<i32>,
) -> ChatRequest {
    let mut messages = Vec::new();

    if !system_prompt.is_empty() {
        messages.push(Message {
            role: "system".to_string(),
            content: system_prompt.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
    }

    // Include conversation history, excluding any in-progress empty assistant message
    // and any stray system messages (system must be first per OpenAI API spec).
    let conv = conversation.lock().unwrap();
    for msg in &*conv {
        // Skip empty assistant messages that are being accumulated during streaming
        if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
            continue;
        }
        // Skip system messages — a fresh one is already prepended above.
        // Stray system messages in history cause the server to reject with
        // "System message must be at the beginning."
        if msg.role == "system" {
            continue;
        }
        messages.push(msg.clone());
    }
    drop(conv);

    messages.push(Message {
        role: "user".to_string(),
        content: prompt.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });

    let (reasoning_effort, chat_template_kwargs) = reasoning_wire(reasoning_effort);
    let (reasoning_format, reasoning_budget_tokens) =
        reasoning_budget_wire(reasoning_format, reasoning_budget_tokens);
    ChatRequest {
        model: "local".to_string(),
        messages,
        stream,
        tools: tools.map(|t| t.to_vec()),
        reasoning_effort,
        chat_template_kwargs,
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        // Live prompt-processing progress is only meaningful for streams.
        return_progress: if stream { Some(true) } else { None },
        reasoning_format,
        reasoning_budget_tokens,
    }
}

/// Send a non-streaming HTTP request and return the completed call's
/// results (content, server-reported usage + model, tool-call count and
/// thinking-character count — see [`NonStreamResult`]).
///
/// Generic over the request shape so callers can serialize either the owned
/// [`ChatRequest`] or the borrowed [`ChatRequestRef`] (perf P2).
pub async fn send_message<R: Serialize>(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    request: &R,
) -> Result<NonStreamResult, Error> {
    let body = serde_json::to_string(request)?;

    let url = format!("{}/v1/chat/completions", base_url);
    let auth = api_key.map(|k| format!("Bearer {}", k));
    let resp = send_with_transient_retries(
        || {
            let mut b = http_client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body.clone());
            if let Some(ref key) = auth {
                b = b.header("Authorization", key.clone());
            }
            async { b.send().await }
        },
        None,
    )
    .await?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| Error::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(llm_error_or_http(status.as_u16(), &text));
    }

    let response: Response = serde_json::from_str(&text)?;

    if response.choices.is_empty() {
        return Err(Error::Stream("Empty response".to_string()));
    }

    let msg = &response.choices[0].message;
    let content = msg.content.clone();
    // llama.cpp reports speeds in a `timings` field SIBLING to `usage`;
    // fold it into the Usage so the UI can show tokens/sec.
    let mut usage = response.usage.clone();
    if let (Some(u), Some(t)) = (usage.as_mut(), response.timings.clone()) {
        if u.timings.is_none() {
            u.timings = Some(t);
        }
    }
    let model = response.model.clone();
    let tool_calls = msg.tool_calls.as_ref().map(|t| t.len() as u32).unwrap_or(0);
    let thinking_chars = msg
        .reasoning_content
        .as_ref()
        .map(|r| r.chars().count() as u64)
        .unwrap_or(0);
    let finish_reason = response.choices[0].finish_reason.clone();
    tracing::debug!(
        "send_message (non-stream) assistant content (len={}) finish_reason={:?} tool_calls={} thinking_chars={}: {:?}",
        content.len(),
        finish_reason,
        tool_calls,
        thinking_chars,
        content.chars().take(200).collect::<String>()
    );

    Ok(NonStreamResult {
        content,
        usage,
        model,
        tool_calls,
        thinking_chars,
    })
}

/// Build the HTTP request builder for a streaming call.
pub fn build_stream_request(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    request: &ChatRequest,
) -> reqwest::RequestBuilder {
    let body = serde_json::to_string(request).unwrap();

    let mut builder = http_client
        .post(format!("{}/v1/chat/completions", base_url))
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .body(body);
    if let Some(ref key) = api_key {
        builder = builder.header("Authorization", format!("Bearer {}", key));
    }
    builder
}

/// Parse the `count` field of a llama.cpp `input_tokens` response
/// (`{"count": N}`). `None` when the body is not JSON or lacks a numeric
/// `count`.
pub fn parse_input_tokens_count(body: &str) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("count").and_then(|c| c.as_u64()).map(|c| c as u32)
}

/// Count the EXACT input tokens of a request via llama.cpp's
/// `POST /v1/chat/completions/input_tokens` (the model's real tokenizer,
/// no generation). The body is the same chat-completion shape as the real
/// call (so `tools` and the full message list are counted the same way);
/// the response is `{"count": N}`.
///
/// Servers without the endpoint (404) or that reject it (501) yield
/// [`Error::UnsupportedEndpoint`] so the caller can feature-detect ONCE and
/// permanently fall back to the char-count estimate.
pub async fn count_input_tokens(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    request: &ChatRequestRef<'_>,
) -> Result<u32, Error> {
    let body = request.to_json()?;
    let mut builder = http_client
        .post(format!("{}/v1/chat/completions/input_tokens", base_url))
        .header("Content-Type", "application/json")
        .body(body);
    if let Some(ref key) = api_key {
        builder = builder.header("Authorization", format!("Bearer {}", key));
    }
    let resp = builder.send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.as_u16() == 404 || status.as_u16() == 501 {
        return Err(Error::UnsupportedEndpoint(text));
    }
    if !status.is_success() {
        return Err(Error::Http(format!("Server returned {}: {}", status, text)));
    }
    parse_input_tokens_count(&text).ok_or_else(|| {
        Error::Json(
            <serde_json::Error as serde::de::Error>::custom(
                "input_tokens response is missing a numeric 'count' field",
            ),
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    const OVERFLOW_BODY: &str = r#"{"error":{"type":"exceed_context_size_error","message":"Prompt is too long: 5242880 tokens > 4096 context size","n_prompt_tokens":5242880,"n_ctx":4096}}"#;

    /// Minimal HTTP/1.1 mock: answers `statuses.len()` sequential connections
    /// with the given status codes in order, one request per connection
    /// (`Connection: close` keeps reqwest from pooling).
    async fn mock_status_server(listener: tokio::net::TcpListener, statuses: Vec<u16>) {
        tokio::spawn(async move {
            for status in statuses {
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    503 => "Service Unavailable",
                    other => panic!("unhandled status {other} in mock"),
                };
                let body = if status == 200 {
                    r#"{"id":"1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#
                } else {
                    r#"{"error":{"type":"server_error","message":"Model not loaded"}}"#
                };
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
                // Give the client time to read the response before the socket
                // closes.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
    }

    #[test]
    fn llm_error_parse_overflow() {
        let e = LlmError::parse(400, OVERFLOW_BODY).expect("envelope parses");
        assert_eq!(e.code, 400);
        assert_eq!(e.error_type, "exceed_context_size_error");
        assert!(!e.is_retryable());
        let ov = e.context_overflow().expect("overflow details");
        assert_eq!(
            ov,
            crate::trimming::ContextOverflow {
                n_prompt: 5_242_880,
                n_ctx: 4096
            }
        );
    }

    #[test]
    fn llm_error_parse_503_is_retryable() {
        let e = LlmError::parse(
            503,
            r#"{"error":{"type":"server_error","message":"Model not loaded"}}"#,
        )
        .unwrap();
        assert!(e.is_retryable());
        assert!(e.context_overflow().is_none());
    }

    #[test]
    fn llm_error_parse_non_json_is_none() {
        assert!(LlmError::parse(500, "Internal Server Error").is_none());
    }

    #[test]
    fn llm_error_parse_error_string_shape() {
        let e = LlmError::parse(404, r#"{"error":"not found"}"#).unwrap();
        assert_eq!(e.message, "not found");
        assert!(e.error_type.is_empty());
        assert!(e.context_overflow().is_none());
    }

    #[test]
    fn llm_error_parse_overflow_type_without_tokens_is_none() {
        let e = LlmError::parse(
            400,
            r#"{"error":{"type":"exceed_context_size_error","message":"too long"}}"#,
        )
        .unwrap();
        assert!(e.context_overflow().is_none());
    }

    #[test]
    fn llm_error_display_includes_type_and_message() {
        let e = LlmError::parse(
            503,
            r#"{"error":{"type":"server_error","message":"Model not loaded"}}"#,
        )
        .unwrap();
        let s = format!("{e}");
        assert!(s.contains("503"), "display: {s}");
        assert!(s.contains("server_error"), "display: {s}");
        assert!(s.contains("Model not loaded"), "display: {s}");
    }

    #[test]
    fn llm_error_or_http_falls_back_to_raw_string() {
        let err = llm_error_or_http(500, "plain text body");
        assert!(
            matches!(err, Error::Http(ref s) if s.contains("plain text body")),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn retries_503_then_succeeds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        mock_status_server(listener, vec![503, 503, 200]).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/chat/completions");
        let resp = send_with_transient_retries(
            || async { client.post(&url).body("{}").send().await },
            None,
        )
        .await
        .expect("transient 503s are retried into success");
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn does_not_retry_400() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Only ONE connection is served: if the code retried, the 2nd
        // request would hang on an empty listener (test timeout).
        mock_status_server(listener, vec![400]).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/chat/completions");
        let resp = send_with_transient_retries(
            || async { client.post(&url).body("{}").send().await },
            None,
        )
        .await
        .expect("non-transient status is returned, not an error");
        assert_eq!(resp.status().as_u16(), 400);
    }

    #[tokio::test]
    async fn exhausted_retries_report_structured_503() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        mock_status_server(listener, vec![503; MAX_TRANSIENT_RETRIES as usize + 1]).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/chat/completions");
        let err = send_with_transient_retries(
            || async { client.post(&url).body("{}").send().await },
            None,
        )
        .await
        .unwrap_err();
        match err {
            Error::Llm(llm) => {
                assert_eq!(llm.code, 503);
                assert_eq!(llm.error_type, "server_error");
            }
            other => panic!("expected Llm error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn connection_refused_is_retried_then_reported() {
        // Bind + drop the listener: the port is closed, so every attempt is
        // ECONNREFUSED (is_connect) and all retries are consumed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/chat/completions");
        let err = send_with_transient_retries(
            || async { client.post(&url).body("{}").send().await },
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, Error::Http(_)),
            "expected Http error for refused connection, got: {err:?}"
        );
    }
}
