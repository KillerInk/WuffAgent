use serde::{Deserialize, Serialize};

use crate::types::{Message, Usage};
use super::Error;

/// HTTP request building and sending for ChatClient.

#[derive(Serialize, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<crate::tools::ToolDefinition>>,
    /// Reasoning effort for reasoning models (omitted when Off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Request options for streaming (e.g. include_usage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Serialize, Debug)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Deserialize, Debug)]
pub struct Response {
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
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
    });

    ChatRequest {
        model: "local".to_string(),
        messages,
        stream,
        tools: tools.map(|t| t.to_vec()),
        reasoning_effort: reasoning_effort.as_wire_value().map(|s| s.to_string()),
        stream_options: Some(StreamOptions { include_usage: true }),
    }
}

/// Send a non-streaming HTTP request and return the response content.
pub async fn send_message(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    request: &ChatRequest,
) -> Result<(String, Option<Usage>), Error> {
    let body = serde_json::to_string(request)?;

    let mut builder = http_client
        .post(format!("{}/v1/chat/completions", base_url))
        .header("Content-Type", "application/json")
        .body(body);
    if let Some(ref key) = api_key {
        builder = builder.header(
            "Authorization",
            format!("Bearer {}", key),
        );
    }

    let resp = builder.send().await?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(Error::Http(format!(
            "Server returned {}: {}",
            status, text
        )));
    }

    let response: Response = serde_json::from_str(&text)?;

    if response.choices.is_empty() {
        return Err(Error::Stream("Empty response".to_string()));
    }

    let content = response.choices[0].message.content.clone();
    let usage = response.usage.clone();
    let finish_reason = response.choices[0].finish_reason.clone();
    tracing::debug!(
        "send_message (non-stream) assistant content (len={}) finish_reason={:?}: {:?}",
        content.len(),
        finish_reason,
        content.chars().take(200).collect::<String>()
    );

    Ok((content, usage))
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
        builder = builder.header(
            "Authorization",
            format!("Bearer {}", key),
        );
    }
    builder
}
