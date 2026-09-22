use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::Error;
use crate::types::{Message, PromptProgress, ToolCall, Usage};

/// Tracks, across one stream, which tool call is currently receiving deltas.
/// This is what lets us detect the moment the model has "moved past" a tool
/// call (a text/reasoning delta arrives, or a delta for a different call
/// arrives) — at that point the call's arguments are complete, so the caller
/// can start executing it while the model keeps reasoning.
///
/// Create one fresh `ToolCallTracker` per stream.
#[derive(Default)]
pub struct ToolCallTracker {
    /// Id of the tool call last touched by a delta.
    active: Option<String>,
    /// Ids already reported to the ready callback.
    ready: HashSet<String>,
}

/// Cheap structural check that a JSON object string is complete: braces and
/// brackets are balanced and we are not left inside a string. Not a full
/// parse — just enough to avoid firing "tool call ready" while the
/// arguments are still streaming.
pub fn looks_like_complete_json(s: &str) -> bool {
    let s = s.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return false;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape = true,
            '"' => in_string = !in_string,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    !in_string && depth == 0
}

/// Report the tool call with the given id via `on_tool_call_ready`, but only
/// if it has not been reported yet and its accumulated arguments already look
/// like complete JSON.
fn fire_tool_ready_if_complete<G: FnMut(ToolCall)>(
    tracker: &mut ToolCallTracker,
    on_tool_call_ready: &mut G,
    conversation: &Arc<Mutex<Vec<Message>>>,
    id: &str,
) {
    if tracker.ready.contains(id) {
        return;
    }
    let call = conversation.lock().ok().and_then(|conv| {
        conv.iter()
            .rev()
            .find(|m| m.role == "assistant")
            .and_then(|m| m.tool_calls.as_ref())
            .and_then(|tcs| tcs.iter().find(|c| c.id == id))
            .cloned()
    });
    if let Some(c) = call {
        if looks_like_complete_json(&c.function.arguments) {
            tracker.ready.insert(id.to_string());
            on_tool_call_ready(c);
        }
    }
}

/// Process a single SSE line and update the conversation.
/// Returns Some(Usage) when the final usage chunk is encountered.
/// The callback receives (content, is_thinking) where `is_thinking` indicates
/// whether this chunk is part of the model's thinking/reasoning output.
///
/// `on_tool_call_ready` is invoked (mid-stream) the moment a tool call is
/// complete and the model has moved past it, so callers can start executing
/// tools while the model keeps reasoning. `tracker` carries the per-stream
/// bookkeeping across lines (one fresh `ToolCallTracker` per stream).
pub async fn process_sse_line(
    line: &str,
    callback: &mut impl FnMut(String, bool) -> Result<(), Error>,
    conversation: &Arc<Mutex<Vec<Message>>>,
    on_tool_call_ready: &mut impl FnMut(ToolCall),
    on_prompt_progress: &mut impl FnMut(PromptProgress),
    tracker: &mut ToolCallTracker,
) -> Result<Option<Usage>, Error> {
    // Callers pass lines including the trailing newline (stream_message slices
    // up to and including '\n'); strip line endings so the comparisons below
    // still match — the usual stream-end frame arrives as "data: [DONE]\n".
    let line = line.trim_end_matches(|c| c == '\n' || c == '\r');
    if line.is_empty() {
        return Ok(None);
    }

    // Extract the payload after the SSE "data:" prefix. The standard
    // OpenAI-compatible format uses "data: " (with a space); some servers
    // omit the space, so accept both.
    let data = if let Some(d) = line.strip_prefix("data: ") {
        d
    } else if let Some(d) = line.strip_prefix("data:") {
        d
    } else {
        // SSE event headers (event:/id:/retry:), comments (:) — ignore.
        return Ok(None);
    };

    // Standard stream-end sentinel (OpenAI-compatible servers).
    if data.trim() == "[DONE]" {
        return Ok(None);
    }

    // Some servers emit non-JSON data lines (keep-alives, partial frames).
    // Skip them instead of failing the whole stream.
    let chunk = match serde_json::from_str::<Value>(data) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                "SSE: skipping unparseable data line (len={}): {:?} (error: {})",
                data.len(),
                &data[..data.len().min(120)],
                e
            );
            return Ok(None);
        }
    };

    // llama.cpp live prompt-processing progress (`return_progress: true`):
    // sent per server tick while the prompt is being processed, before the
    // first token. Forward it to the caller (status bar live PP speed).
    // Tolerant parse — backends that don't send it are simply unaffected.
    if let Some(pp) = chunk
        .get("prompt_progress")
        .and_then(|p| serde_json::from_value::<PromptProgress>(p.clone()).ok())
    {
        on_prompt_progress(pp);
    }

    // ── Early tool-call detection ────────────────────────────────────────
    // A tool call is ready to execute as soon as the stream moves PAST it:
    // a text/reasoning delta arrives, or a delta for a different call does.
    // At that point its arguments are complete, so the caller can start
    // executing it while the model keeps reasoning. The very last tool call
    // in a stream is never reported (nothing follows it to signal
    // completion) — callers execute that one inline.
    let has_text = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .map(|d| {
            ["content", "thinking", "reasoning", "reasoning_content"]
                .iter()
                .any(|k| {
                    d.get(*k)
                        .and_then(|v| v.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                })
        })
        .unwrap_or(false);
    // Id of the tool call touched by THIS line (if any). Continuation
    // chunks carry only an index — resolve it to the id of the call
    // already accumulated at that slot.
    let incoming_id: Option<String> = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("tool_calls"))
        .and_then(|t| t.as_array())
        .and_then(|tcs| tcs.first())
        .and_then(|tc| {
            tc.get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    tc.get("index").and_then(|v| v.as_u64()).and_then(|idx| {
                        conversation
                            .lock()
                            .ok()?
                            .last()?
                            .tool_calls
                            .as_ref()?
                            .get(idx as usize)
                            .map(|c| c.id.clone())
                    })
                })
        });

    if has_text {
        // (id cloned so the mutable `tracker` borrow below is allowed)
        if let Some(active_id) = tracker.active.clone() {
            fire_tool_ready_if_complete(tracker, on_tool_call_ready, conversation, &active_id);
        }
    }
    if let Some(ref incoming) = incoming_id {
        if tracker.active.as_deref() != Some(incoming.as_str()) {
            if let Some(active_id) = tracker.active.clone() {
                fire_tool_ready_if_complete(tracker, on_tool_call_ready, conversation, &active_id);
            }
            tracker.active = Some(incoming.clone());
        }
    }

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

    // Handle thinking/reasoning content in streaming delta chunks
    // Claude uses `delta.thinking`, llama.cpp uses `delta.reasoning` or
    // `delta.reasoning_content` (deepseek reasoning format, Qwen3 default)
    let thinking = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("thinking"))
        .and_then(|t| t.as_str())
        .or_else(|| {
            chunk
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("reasoning"))
                .and_then(|t| t.as_str())
        })
        .or_else(|| {
            chunk
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("reasoning_content"))
                .and_then(|t| t.as_str())
        });
    if let Some(thinking) = thinking {
        if !thinking.is_empty() {
            tracing::trace!(
                "SSE: thinking/reasoning chunk received (len={}), source=delta.thinking or delta.reasoning",
                thinking.len()
            );
            callback(thinking.to_string(), true)?;

            // Accumulate into the conversation so the model's reasoning
            // round-trips on the next request (improves tool-call reliability
            // with reasoning models such as Qwen3/DeepSeek).
            let mut conv = conversation.lock().unwrap();
            if let Some(last) = conv.last_mut() {
                match last.reasoning_content.as_mut() {
                    Some(r) => r.push_str(thinking),
                    None => last.reasoning_content = Some(thinking.to_string()),
                }
            }
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
                let id = tc_chunk.get("id").and_then(|v| v.as_str());
                // Extract index (used when id is not present)
                let index = tc_chunk.get("index").and_then(|v| v.as_u64());
                let func = tc_chunk.get("function");

                if let Some(func) = func {
                    let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("");

                    // Accumulate partial args from streaming
                    let mut conv = conversation.lock().unwrap();
                    if let Some(last) = conv.last_mut() {
                        if last.tool_calls.is_none() {
                            last.tool_calls = Some(Vec::new());
                        }
                        let tcs = last.tool_calls.as_mut().unwrap();

                        // Try to find existing tool call by id first
                        let found = if let Some(ref id) = id {
                            tcs.iter_mut().find(|t| t.id == *id)
                        } else {
                            None
                        };

                        if let Some(tc) = found {
                            // Accumulate args into existing tool call
                            tc.function.arguments.push_str(args);
                        } else if let Some(idx) = index {
                            // Find by index when id is not present
                            if let Some(tc) = tcs.get_mut(idx as usize) {
                                tc.function.arguments.push_str(args);
                            } else if let Some(id) = id {
                                // Index doesn't exist yet but we have an id - create new tool call
                                tcs.push(crate::types::ToolCall {
                                    id: id.to_string(),
                                    call_type: "function".to_string(),
                                    function: crate::types::ToolFunction {
                                        name: name.to_string(),
                                        arguments: args.to_string(),
                                    },
                                });
                            } else {
                                tracing::warn!(
                                    "process_sse_line: failed to find tool_call at index={} (tcs_len={})",
                                    idx, tcs.len()
                                );
                            }
                        } else if let Some(id) = id {
                            // Create new tool call
                            tcs.push(crate::types::ToolCall {
                                id: id.to_string(),
                                call_type: "function".to_string(),
                                function: crate::types::ToolFunction {
                                    name: name.to_string(),
                                    arguments: args.to_string(),
                                },
                            });
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
    if let Some(usage_val) = chunk.get("usage") {
        if let Some(mut usage) = serde_json::from_value::<Usage>(usage_val.clone()).ok() {
            // llama.cpp reports speeds in a `timings` field SIBLING to
            // `usage` in the final chunk; fold it into the Usage.
            if usage.timings.is_none() {
                usage.timings = chunk
                    .get("timings")
                    .and_then(|t| serde_json::from_value(t.clone()).ok());
            }
            return Ok(Some(usage));
        }
    }

    Ok(None)
}

/// Extract the `model` field from a raw SSE line (e.g.
/// `data: {"model":"deepseek-chat",...}`). Returns `None` for non-data
/// lines, `[DONE]`, or chunks without a model name.
pub fn extract_model(line: &str) -> Option<String> {
    let data = line.trim().strip_prefix("data:")?.trim();
    if data == "[DONE]" {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(data)
        .ok()?
        .get("model")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Stream a message and process SSE lines.
/// `on_tool_call_ready` is invoked (mid-stream) the moment a tool call is
/// complete and the model has moved past it, so the caller can start
/// executing it while the stream continues (tools run while the model keeps
/// reasoning). Pass one fresh `ToolCallTracker` per stream.
/// `conversation` is updated in place with the new messages.
/// `cancel_token` can be used to abort the stream early (e.g. user cancellation).
/// Pass `None` to disable cancellation for this stream.
///
/// Returns `(usage, model)`: the server-reported usage (when the backend
/// sends it) and the model name from the first chunk that carried one
/// (for the usage log).
pub async fn stream_message(
    resp: reqwest::Response,
    conversation: &Arc<Mutex<Vec<Message>>>,
    callback: &mut (impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static),
    on_tool_call_ready: &mut (impl FnMut(ToolCall) + Send + Sync + 'static),
    on_prompt_progress: &mut (impl FnMut(PromptProgress) + Send + Sync + 'static),
    tracker: &mut ToolCallTracker,
    cancel_token: Option<&CancellationToken>,
) -> Result<(Option<Usage>, Option<String>), Error> {
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut last_usage: Option<Usage> = None;
    let mut model: Option<String> = None;

    if let Some(cancel_token) = cancel_token {
        return tokio::select! {
            result = async {
                while let Some(chunk) = stream.next().await {
                    let bytes = chunk?;
                    buffer.push_str(&String::from_utf8_lossy(&bytes));

                    // Process complete lines
                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..=newline_pos].to_string();
                        buffer.drain(..=newline_pos);
                        // Capture the model once (usually from the very
                        // first chunk); skip the extra JSON parse after that.
                        if model.is_none() {
                            model = extract_model(&line);
                        }
                        if let Some(usage) = process_sse_line(
                            &line,
                            callback,
                            conversation,
                            on_tool_call_ready,
                            on_prompt_progress,
                            tracker,
                        )
                        .await?
                        {
                            last_usage = Some(usage);
                        }
                    }
                }
                Ok::<_, Error>((last_usage, model))
            } => result,
            _ = cancel_token.cancelled() => {
                Err(Error::Cancelled)
            }
        };
    } else {
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete lines
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..=newline_pos].to_string();
                buffer.drain(..=newline_pos);
                if model.is_none() {
                    model = extract_model(&line);
                }
                if let Some(usage) = process_sse_line(
                    &line,
                    callback,
                    conversation,
                    on_tool_call_ready,
                    on_prompt_progress,
                    tracker,
                )
                .await?
                {
                    last_usage = Some(usage);
                }
            }
        }
    }

    Ok((last_usage, model))
}

/// Add user and empty assistant messages to the conversation.
pub fn add_streaming_messages(conversation: &Arc<Mutex<Vec<Message>>>, prompt: &str) {
    let mut conv = conversation.lock().unwrap();
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
        content: String::new(),
        timestamp: crate::types::format_timestamp(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });
}
