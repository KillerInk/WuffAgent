use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::Value;

use crate::types::{Message, Usage};
use super::Error;

/// Process a single SSE line and update the conversation.
/// Returns Some(Usage) when the final usage chunk is encountered.
/// The callback receives (content, is_thinking) where `is_thinking` indicates
/// whether this chunk is part of the model's thinking/reasoning output.
pub async fn process_sse_line(
    line: &str,
    callback: &mut impl FnMut(String, bool) -> Result<(), Error>,
    conversation: &Arc<Mutex<Vec<Message>>>,
) -> Result<Option<Usage>, Error> {
    if line.is_empty() || line == "data: [DONE]" {
        return Ok(None);
    }

    if !line.starts_with("data: ") {
        return Ok(None);
    }

    let data = &line["data: ".len()..];
    // Log the raw SSE data for debugging tool call issues
    if data.contains("tool_calls") || data.contains("function") {
        tracing::debug!("process_sse_line raw SSE data: {}", &data[..data.len().min(500)]);
    }
    let chunk: Value = serde_json::from_str(data)?;

    if let Some(text) = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
    {
        if !text.is_empty() {
            callback(text.to_string(), false)?;

            // Update conversation history
            let mut conv = conversation.lock().unwrap();
            if let Some(last) = conv.last_mut() {
                last.content.push_str(text);
            }
        }
    }

    // Handle thinking content in streaming delta chunks (e.g. Claude-style reasoning)
    if let Some(thinking) = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("thinking"))
        .and_then(|t| t.as_str())
    {
        if !thinking.is_empty() {
            tracing::debug!("process_sse_line: thinking chunk received, len={}", thinking.len());
            callback(thinking.to_string(), true)?;
        }
    }

    // Handle tool_calls in streaming delta chunks
    if let Some(tool_calls) = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("tool_calls"))
    {
        if let Some(tc_array) = tool_calls.as_array() {
            for tc_chunk in tc_array {
                // Extract id (may be null/missing in delta chunks after the first)
                let id = tc_chunk
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                // Extract index (used when id is not present)
                let index = tc_chunk.get("index").and_then(|v| v.as_u64());
                let func = tc_chunk.get("function");

                if let Some(func) = func {
                    let name = func
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = func
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    tracing::debug!(
                        "process_sse_line: tool_call id=? name={} args_len={} args_preview={:?}",
                        name, args.len(),
                        &args[..args.len().min(80)]
                    );

                    // Accumulate partial args from streaming
                    let mut conv = conversation.lock().unwrap();
                    if let Some(last) = conv.last_mut() {
                        tracing::debug!(
                            "process_sse_line: last message role={}, has_tool_calls={}, tool_calls_count={}",
                            last.role,
                            last.tool_calls.is_some(),
                            last.tool_calls.as_ref().map(|t| t.len()).unwrap_or(0)
                        );
                        if last.tool_calls.is_none() {
                            last.tool_calls = Some(Vec::new());
                        }
                        let tcs = last.tool_calls.as_mut().unwrap();

                        tracing::debug!(
                            "process_sse_line: looking for tool_call id={:?} index={} tcs_len={}",
                            id,
                            index.unwrap_or(0),
                            tcs.len()
                        );

                        // Try to find existing tool call by id first
                        let found = if let Some(ref id) = id {
                            tcs.iter_mut().find(|t| t.id == *id)
                        } else {
                            None
                        };

                        if let Some(tc) = found {
                            // Accumulate args into existing tool call
                            tc.function.arguments.push_str(&args);
                            tracing::debug!(
                                "process_sse_line: accumulated args for tool_call id={} total_len={} args={:?}",
                                id.as_ref().unwrap(), tc.function.arguments.len(),
                                &tc.function.arguments[..tc.function.arguments.len().min(80)]
                            );
                        } else if let Some(idx) = index {
                            // Find by index when id is not present
                            if let Some(tc) = tcs.get_mut(idx as usize) {
                                tc.function.arguments.push_str(&args);
                                tracing::debug!(
                                    "process_sse_line: accumulated args for tool_call index={} total_len={} args={:?}",
                                    idx, tc.function.arguments.len(),
                                    &tc.function.arguments[..tc.function.arguments.len().min(80)]
                                );
                            } else if let Some(ref id) = id {
                                // Index doesn't exist yet but we have an id - create new tool call
                                tcs.push(crate::types::ToolCall {
                                    id: id.to_string(),
                                    call_type: "function".to_string(),
                                    function: crate::types::ToolFunction {
                                        name,
                                        arguments: args.clone(),
                                    },
                                });
                                tracing::debug!(
                                    "process_sse_line: created new tool_call id={} at index={} args={:?}",
                                    id, idx, args
                                );
                            } else {
                                tracing::warn!(
                                    "process_sse_line: failed to find tool_call at index={} (tcs_len={})",
                                    idx, tcs.len()
                                );
                            }
                        } else if let Some(ref id) = id {
                            // Create new tool call
                            tcs.push(crate::types::ToolCall {
                                id: id.to_string(),
                                call_type: "function".to_string(),
                                function: crate::types::ToolFunction {
                                    name,
                                    arguments: args.clone(),
                                },
                            });
                            tracing::debug!(
                                "process_sse_line: created new tool_call id={} args={:?}",
                                id, args
                            );
                        } else {
                            tracing::warn!(
                                "process_sse_line: skipping chunk with no id and no index"
                            );
                        }
                        // If no id and no index, skip this chunk
                    } else {
                        tracing::warn!("process_sse_line: no last message in conversation");
                    }
                }
            }
        }
    }

    // Extract usage from the final chunk (when choices has no delta but has usage)
    if let Some(usage) = chunk.get("usage").and_then(|u| serde_json::from_value(u.clone()).ok()) {
        return Ok(Some(usage));
    }

    Ok(None)
}

/// Stream a message and process SSE lines.
/// `conversation` is updated in place with the new messages.
pub async fn stream_message(
    resp: reqwest::Response,
    conversation: &Arc<Mutex<Vec<Message>>>,
    callback: &mut (impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static),
) -> Result<Option<Usage>, Error> {
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut last_usage: Option<Usage> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // Process complete lines
        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].to_string();
            buffer = buffer[newline_pos + 1..].to_string();

            if let Some(usage) = process_sse_line(&line, callback, conversation).await? {
                last_usage = Some(usage);
            }
        }
    }

    Ok(last_usage)
}

/// Stream a message using an Arc-based approach (for use from async contexts
/// where the ChatClient is wrapped in Arc<Mutex<>>).
/// `conversation` is the cloned Arc to the conversation lock.
pub async fn stream_message_arc(
    resp: reqwest::Response,
    conversation: &Arc<Mutex<Vec<Message>>>,
    callback: &mut (impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static),
) -> Result<Option<Usage>, Error> {
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut last_usage: Option<Usage> = None;
    let mut cb = callback;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].to_string();
            buffer = buffer[newline_pos + 1..].to_string();

            if line.starts_with("data: ") && !line.contains("[DONE]") {
                tracing::trace!("stream_message (arc) SSE line: {}", &line["data: ".len()..].chars().take(200).collect::<String>());
            }

            if let Some(usage) = process_sse_line(&line, &mut cb, conversation).await? {
                last_usage = Some(usage);
            }
        }
    }

    tracing::debug!("stream_message (arc) streaming completed, usage={:?}", last_usage);
    Ok(last_usage)
}

/// Add user and empty assistant messages to the conversation.
pub fn add_streaming_messages(conversation: &Arc<Mutex<Vec<Message>>>, prompt: &str) {
    let mut conv = conversation.lock().unwrap();
    conv.push(Message {
        role: "user".to_string(),
        content: prompt.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
    });
    conv.push(Message {
        role: "assistant".to_string(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
    });
}
