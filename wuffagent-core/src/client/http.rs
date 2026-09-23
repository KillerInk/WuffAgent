use serde::{Deserialize, Serialize};

use super::Error;
use crate::types::{Message, Usage};

/// HTTP request building and sending for ChatClient.

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
    }
}

/// Send a non-streaming HTTP request and return the completed call's
/// results (content, server-reported usage + model, tool-call count and
/// thinking-character count — see [`NonStreamResult`]).
pub async fn send_message(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    request: &ChatRequest,
) -> Result<NonStreamResult, Error> {
    let body = serde_json::to_string(request)?;

    let mut builder = http_client
        .post(format!("{}/v1/chat/completions", base_url))
        .header("Content-Type", "application/json")
        .body(body);
    if let Some(ref key) = api_key {
        builder = builder.header("Authorization", format!("Bearer {}", key));
    }

    let resp = builder.send().await?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| Error::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(Error::Http(format!("Server returned {}: {}", status, text)));
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
