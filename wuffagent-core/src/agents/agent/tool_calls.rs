//! Tool-call execution sections of `run_llm_loop` (extracted A2):
//!
//! - `run_native_tool_calls` — the native tool-call section: after the
//!   stream ends, every non-truncated call is guaranteed to be running
//!   (early-started ones already are; the rest are started now, in
//!   parallel), then results are collected in CALL order.
//! - `run_text_embedded_calls` — the text-embedded fallback for models that
//!   do not honor native function calling: all calls run in parallel,
//!   collected in call order.
//!
//! Both append the resulting messages to the request list AND the shared
//! store (request list + store must stay in lockstep).

use std::collections::HashSet;

use tokio_util::sync::CancellationToken;

use crate::tools::{ToolDefinition, ToolManager};
use crate::types::{Message, ToolCall};

use super::tool_exec::PendingToolRuns;
use super::Agent;

/// Run the round's NATIVE tool calls and record the tool results (request
/// list + shared store).
///
/// FULL PARALLEL: every non-truncated call runs in the background. The
/// model's "moved past" calls were early-started mid-stream (ready
/// callback); pass 1 starts the rest — typically the LAST call in the
/// stream, which never got that signal — and pass 2 collects ALL results
/// in CALL order (completions may arrive out of order; the UI keys cards
/// on call_id).
pub(crate) async fn run_native_tool_calls(
    agent: &Agent,
    calls: &[ToolCall],
    pending: &PendingToolRuns,
    cancel_token: &CancellationToken,
    truncated_ids: &HashSet<String>,
    messages: &mut Vec<Message>,
    tool_manager: &ToolManager,
) -> Result<(), String> {
    // ── Pass 1 (no await): start whatever is not running yet ────────────
    // `start` is a no-op for ids that are already in the map, so
    // early-started calls keep their (younger) runs. Truncated calls are
    // never executable — they are reported, not started.
    for call in calls {
        if !truncated_ids.contains(&call.id) {
            pending.start(call);
        }
    }

    // ── Pass 2: collect in call order ───────────────────────────────────
    for call in calls {
        if cancel_token.is_cancelled() {
            // Stop runs that have not been collected yet so nothing keeps
            // executing in the background for a dead turn.
            pending.abort_all();
            return Err("Cancelled".to_string());
        }
        if truncated_ids.contains(&call.id) {
            // Truncated mid-stream: the call cannot be executed. Abort a
            // started run (defensive — the ready callback only fires for
            // complete arguments) and report the truncation as the tool
            // result so the model retries with a smaller payload.
            if let Some(handle) = pending.take(&call.id) {
                handle.abort();
            } else {
                agent.send_event(crate::types::AppEvent::ToolCallStart {
                    tool_name: call.function.name.clone(),
                    call_id: call.id.clone(),
                    args_preview: "(arguments truncated by model output limit)".to_string(),
                    session_id: agent.session_id(),
                });
            }
            let result_str = format!(
                "Error: tool call arguments for '{}' were truncated (the model ran out of output tokens mid-argument), so the tool did not execute. Retry with a smaller payload - e.g. split the content across multiple tool calls or use a more targeted edit.",
                call.function.name
            );
            agent.send_event(crate::types::AppEvent::ToolCallError {
                tool_name: call.function.name.clone(),
                call_id: call.id.clone(),
                error: result_str.clone(),
                session_id: agent.session_id(),
            });
            let tool_msg = tool_message(&call.id, result_str, None);
            messages.push(tool_msg.clone());
            agent.record_in_store(&tool_msg);
            continue;
        }
        // Bind the handle out of the map before awaiting: the mutex guard
        // must not live across the await (the enclosing future must stay Send).
        let outcome: Result<String, String> = match pending.take(&call.id) {
            Some(handle) => match handle.await {
                Ok(r) => r, // Ok = result text, Err = argument-parse error
                Err(join_err) => {
                    let e = format!("tool task failed: {}", join_err);
                    agent.send_event(crate::types::AppEvent::ToolCallError {
                        tool_name: call.function.name.clone(),
                        call_id: call.id.clone(),
                        error: e.clone(),
                        session_id: agent.session_id(),
                    });
                    Ok(format!("Error: {e}"))
                }
            },
            // Degenerate: empty or duplicate id (the pending map is keyed by
            // id) — run it inline instead.
            None => {
                agent.send_event(crate::types::AppEvent::ToolCallStart {
                    tool_name: call.function.name.clone(),
                    call_id: call.id.clone(),
                    args_preview: crate::tools::tool_args_summary(
                        &call.function.name,
                        &call.function.arguments,
                    ),
                    session_id: agent.session_id(),
                });
                let progress = agent.tool_progress_for(&call.function.name, &call.id);
                super::tool_exec::execute_tool_call(
                    tool_manager,
                    cancel_token,
                    &call.function.name,
                    &call.function.arguments,
                    &progress,
                )
                .await
            }
        };
        let result_str = match outcome {
            Ok(s) => s,
            Err(bad_args) => {
                tracing::warn!(
                    "[AGENT] Bad args for '{}': {}",
                    call.function.name,
                    bad_args
                );
                agent.send_event(crate::types::AppEvent::ToolCallError {
                    tool_name: call.function.name.clone(),
                    call_id: call.id.clone(),
                    error: bad_args.clone(),
                    session_id: agent.session_id(),
                });
                format!("Error: {bad_args}")
            }
        };
        // The UI card gets the FULL result (it renders the image from the
        // `data_uri` field); completions are emitted in call order.
        agent.send_event(crate::types::AppEvent::ToolCallComplete {
            tool_name: call.function.name.clone(),
            call_id: call.id.clone(),
            result: result_str.clone(),
            session_id: agent.session_id(),
        });
        // The MODEL gets show_image's picture as a real image part instead
        // of a base64 text blob (see tool_msg_image_fields).
        let (msg_content, msg_image) = tool_msg_image_fields(&call.function.name, &result_str);
        let tool_msg = tool_message(&call.id, msg_content, msg_image);
        messages.push(tool_msg.clone());
        agent.record_in_store(&tool_msg);
    }
    Ok(())
}

/// A `role: "tool"` result message (tools must always return *something*: an
/// empty result string becomes an empty tool message, which the model/server
/// rejects — callers normalize before calling this).
fn tool_message(call_id: &str, content: String, image: Option<String>) -> Message {
    Message {
        role: "tool".to_string(),
        content,
        timestamp: crate::types::format_timestamp(),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
        reasoning_content: None,
        image,
    }
}

/// Run the round's text-embedded tool calls (non-native models) and record
/// the results (request list + shared store).
///
/// FULL PARALLEL, same shape as the native path: pass 1 starts every call as
/// a background task (via the shared `PendingToolRuns` machinery, so
/// start/progress/cancellation/normalization are identical), pass 2 collects
/// results in CALL order so the assistant/tool message pairs stay ordered.
///
/// Returns `true` when at least one call was executed — the caller must then
/// `continue` the LLM loop.
pub(crate) async fn run_text_embedded_calls(
    agent: &Agent,
    tool_defs: &Option<Vec<ToolDefinition>>,
    display_content: &str,
    cancel_token: &CancellationToken,
    pending: &PendingToolRuns,
    messages: &mut Vec<Message>,
    tool_manager: &ToolManager,
) -> Result<bool, String> {
    if tool_defs.as_ref().map(|d| d.is_empty()).unwrap_or(false) {
        // No tools offered — skip text parsing entirely.
        return Ok(false);
    }
    let mut parsed: Vec<super::toolcall_parse::ToolCall> = Vec::new();
    if let Some(bash_calls) = agent.extract_bash_as_tool_calls(display_content) {
        parsed.extend(bash_calls);
    }
    if parsed.is_empty() {
        if let Some(json_calls) = agent.parse_tool_calls(display_content) {
            parsed.extend(json_calls);
        }
    }
    if parsed.is_empty() {
        return Ok(false);
    }
    // Normalize to the wire-format tool call used by `Message` (the parse
    // type exists only because model text is deserialized into it).
    let embedded: Vec<ToolCall> = parsed.iter().map(|c| c.to_message_call()).collect();

    // ── Pass 1 (no await): start everything in the background ────────────
    // (The old code awaited each call before starting the next, so N blocks
    // took N × the slowest one. `start` is a no-op for empty ids — those
    // are executed inline in pass 2.)
    for call in &embedded {
        pending.start(call);
    }

    // ── Pass 2: collect in call order ────────────────────────────────────
    for call in &embedded {
        if cancel_token.is_cancelled() {
            // Stop runs that have not been collected yet.
            pending.abort_all();
            return Err("Cancelled".to_string());
        }
        // Bind the handle out of the map before awaiting: the mutex guard
        // must not live across the await (the enclosing future must stay Send).
        let outcome: Result<String, String> = match pending.take(&call.id) {
            Some(handle) => match handle.await {
                Ok(r) => r, // Ok = result text, Err = argument-parse error
                Err(join_err) => {
                    let e = format!("tool task failed: {join_err}");
                    agent.send_event(crate::types::AppEvent::ToolCallError {
                        tool_name: call.function.name.clone(),
                        call_id: call.id.clone(),
                        error: e.clone(),
                        session_id: agent.session_id(),
                    });
                    Ok(format!("Error: {e}"))
                }
            },
            // Degenerate: empty or duplicate id (the pending map is keyed by
            // id) — run it inline instead.
            None => {
                agent.send_event(crate::types::AppEvent::ToolCallStart {
                    tool_name: call.function.name.clone(),
                    call_id: call.id.clone(),
                    args_preview: crate::tools::tool_args_summary(
                        &call.function.name,
                        &call.function.arguments,
                    ),
                    session_id: agent.session_id(),
                });
                let progress = agent.tool_progress_for(&call.function.name, &call.id);
                super::tool_exec::execute_tool_call(
                    tool_manager,
                    cancel_token,
                    &call.function.name,
                    &call.function.arguments,
                    &progress,
                )
                .await
            }
        };
        let result_str = match outcome {
            Ok(s) => s,
            Err(bad_args) => {
                tracing::warn!(
                    "[AGENT] Bad args for '{}': {}",
                    call.function.name,
                    bad_args
                );
                agent.send_event(crate::types::AppEvent::ToolCallError {
                    tool_name: call.function.name.clone(),
                    call_id: call.id.clone(),
                    error: bad_args.clone(),
                    session_id: agent.session_id(),
                });
                format!("Error: {bad_args}")
            }
        };
        agent.send_event(crate::types::AppEvent::ToolCallComplete {
            tool_name: call.function.name.clone(),
            call_id: call.id.clone(),
            result: result_str.clone(),
            session_id: agent.session_id(),
        });
        let (msg_content, msg_image) = tool_msg_image_fields(&call.function.name, &result_str);
        // History entry in API-native shape (id links the result).
        let fb_assistant = Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: Some(vec![ToolCall {
                id: call.id.clone(),
                call_type: call.call_type.clone(),
                function: crate::types::ToolFunction {
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        };
        messages.push(fb_assistant.clone());
        agent.record_in_store(&fb_assistant);
        let fb_tool = tool_message(&call.id, msg_content, msg_image);
        messages.push(fb_tool.clone());
        agent.record_in_store(&fb_tool);
    }
    Ok(true)
}

/// Model-facing fields for a tool-result message.
///
/// `show_image` returns the rendered picture as a base64 `data_uri` inside
/// its result JSON. As plain text that payload is unreadable to the model
/// (tens of thousands of base64 "tokens" of noise). So for show_image the
/// base64 is MOVED into the message's `image` field — serialized on the wire
/// as an OpenAI/llama.cpp `image_url` content part, i.e. the same multimodal
/// shape attached user images use — and the JSON text keeps a short
/// placeholder plus the metadata (path, dimensions, size). Other tools and
/// non-JSON results pass through unchanged.
fn tool_msg_image_fields(tool_name: &str, result_str: &str) -> (String, Option<String>) {
    if tool_name != "show_image" {
        return (result_str.to_string(), None);
    }
    let mut value = match serde_json::from_str::<serde_json::Value>(result_str) {
        Ok(v) => v,
        Err(_) => return (result_str.to_string(), None),
    };
    let uri = match value
        .get("data_uri")
        .and_then(|v| v.as_str())
        .filter(|u| u.starts_with("data:image/"))
    {
        Some(u) => u.to_string(),
        None => return (result_str.to_string(), None),
    };
    let kb = value
        .get("bytes")
        .and_then(|v| v.as_u64())
        .map(|b| b / 1024)
        .unwrap_or(0);
    value["data_uri"] =
        serde_json::json!(format!("(sent to the model as an image part; {kb} KB JPEG)"));
    (value.to_string(), Some(uri))
}

#[cfg(test)]
mod tests {
    use super::tool_msg_image_fields;

    #[test]
    fn show_image_result_moves_payload_into_image_part() {
        let result = r#"{"path":"C:/x/y.png","source":"file","format":"png","width":900,"height":506,"bytes":123456,"data_uri":"data:image/jpeg;base64,AAAA"}"#;
        let (content, image) = tool_msg_image_fields("show_image", result);
        assert_eq!(
            image.as_deref(),
            Some("data:image/jpeg;base64,AAAA"),
            "the data URI must move into the image part"
        );
        assert!(!content.contains("AAAA"), "base64 payload must leave the text");
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["path"], "C:/x/y.png");
        assert_eq!(v["width"], 900);
        assert!(v["data_uri"].as_str().unwrap().starts_with("(sent to the model"));
    }

    #[test]
    fn other_tools_pass_through() {
        let (content, image) = tool_msg_image_fields("read_file", r#"{"path":"a.rs"}"#);
        assert_eq!(content, r#"{"path":"a.rs"}"#);
        assert!(image.is_none());
    }

    #[test]
    fn show_image_without_data_uri_passes_through() {
        let (content, image) = tool_msg_image_fields("show_image", "Error: could not read image file");
        assert_eq!(content, "Error: could not read image file");
        assert!(image.is_none());
    }

    #[test]
    fn show_image_non_image_data_uri_passes_through() {
        let result = r#"{"data_uri":"data:text/plain;base64,AAAA"}"#;
        let (content, image) = tool_msg_image_fields("show_image", result);
        assert_eq!(content, result);
        assert!(image.is_none());
    }
}
