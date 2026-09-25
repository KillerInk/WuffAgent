//! Tool-call execution sections of `run_llm_loop` (extracted A2):
//!
//! - `run_native_tool_calls` — the native tool-call loop: collects the
//!   results of early-started runs (the ready callback) in call order and
//!   executes inline whatever never got a "moved past" signal.
//! - `run_text_embedded_calls` — the text-embedded fallback for models that
//!   do not honor native function calling.
//!
//! Both append the resulting messages to the request list AND the shared
//! store (request list + store must stay in lockstep).

use std::collections::HashSet;

use tokio_util::sync::CancellationToken;

use crate::tools::{ToolDefinition, ToolManager};
use crate::types::{Message, ToolCall};

use super::tool_exec::PendingToolRuns;
use super::Agent;

/// Run the round's NATIVE tool calls, in call order, and record the tool
/// results (request list + shared store).
///
/// Calls the model has already moved past were early-started mid-stream
/// (the ready callback) — collect their results here. Calls that never got
/// a "moved past" signal (typically the LAST one in the stream) are executed
/// inline, exactly as before.
pub(crate) async fn run_native_tool_calls(
    agent: &Agent,
    calls: &[ToolCall],
    pending: &PendingToolRuns,
    cancel_token: &CancellationToken,
    truncated_ids: &HashSet<String>,
    messages: &mut Vec<Message>,
    tool_manager: &ToolManager,
) -> Result<(), String> {
    for call in calls {
        if cancel_token.is_cancelled() {
            // Stop early-started runs that have not been collected yet so
            // nothing keeps executing in the background for a dead turn.
            pending.abort_all();
            return Err("Cancelled".to_string());
        }
        if truncated_ids.contains(&call.id) {
            // Truncated mid-stream: the call cannot be executed. Abort an
            // early-started run (defensive — the ready callback only fires
            // for complete arguments) and report the truncation as the tool
            // result so the model retries with a smaller payload.
            let early = pending.take(&call.id);
            if let Some(handle) = early {
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
            let tool_msg = Message {
                role: "tool".to_string(),
                content: result_str,
                timestamp: crate::types::format_timestamp(),
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                reasoning_content: None,
                image: None,
            };
            messages.push(tool_msg.clone());
            agent.record_in_store(&tool_msg);
            continue;
        }
        // Bind the handle out of the map before awaiting: the mutex guard
        // must not live across the await (the enclosing future must stay Send).
        let early_handle = pending.take(&call.id);
        let result_str: String = if let Some(handle) = early_handle {
            match handle.await {
                Ok(Ok(s)) => s,
                Ok(Err(bad_args)) => {
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
                    format!("Error: {}", bad_args)
                }
                Err(join_err) => {
                    let e = format!("tool task failed: {}", join_err);
                    agent.send_event(crate::types::AppEvent::ToolCallError {
                        tool_name: call.function.name.clone(),
                        call_id: call.id.clone(),
                        error: e.clone(),
                        session_id: agent.session_id(),
                    });
                    format!("Error: {}", e)
                }
            }
        } else {
            // Inline fallback: the stream ended while this call was still
            // the active one.
            agent.send_event(crate::types::AppEvent::ToolCallStart {
                tool_name: call.function.name.clone(),
                call_id: call.id.clone(),
                args_preview: crate::tools::tool_args_summary(
                    &call.function.name,
                    &call.function.arguments,
                ),
                session_id: agent.session_id(),
            });
            let params =
                match crate::tools::manager::parse_tool_args(&call.function.arguments) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            "[AGENT] Bad args for '{}': {}",
                            call.function.name,
                            e
                        );
                        agent.send_event(crate::types::AppEvent::ToolCallError {
                            tool_name: call.function.name.clone(),
                            call_id: call.id.clone(),
                            error: e.clone(),
                            session_id: agent.session_id(),
                        });
                        let bad_args_msg = Message {
                            role: "tool".to_string(),
                            content: format!("Error: {}", e),
                            timestamp: crate::types::format_timestamp(),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                            reasoning_content: None,
                            image: None,
                        };
                        messages.push(bad_args_msg.clone());
                        agent.record_in_store(&bad_args_msg);
                        continue;
                    }
                };
            let manager = tool_manager.clone();
            let progress = agent.tool_progress_for(&call.function.name, &call.id);
            let tool_result =
                manager.execute_with_progress(&call.function.name, params, &progress).await;
            // Tools must always return *something*: an empty result
            // string becomes an empty `role: "tool"` message, which
            // the model/server rejects.
            match tool_result {
                Ok(output) => {
                    let s = format!("{}", output);
                    if s.trim().is_empty() {
                        "(no output)".to_string()
                    } else {
                        s
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[AGENT] Tool '{}' failed: {}",
                        call.function.name,
                        e
                    );
                    format!("Error: {}", e)
                }
            }
        };
        // Early-started calls sent their ToolCallStart mid-stream; emit the
        // completion in call order now. The UI card gets the FULL result
        // (it renders the image from the `data_uri` field).
        agent.send_event(crate::types::AppEvent::ToolCallComplete {
            tool_name: call.function.name.clone(),
            call_id: call.id.clone(),
            result: result_str.clone(),
            session_id: agent.session_id(),
        });
        // The MODEL gets show_image's picture as a real image part instead
        // of a base64 text blob (see tool_msg_image_fields).
        let (msg_content, msg_image) = tool_msg_image_fields(&call.function.name, &result_str);
        let tool_msg = Message {
            role: "tool".to_string(),
            content: msg_content,
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: Some(call.id.clone()),
            reasoning_content: None,
            image: msg_image,
        };
        messages.push(tool_msg.clone());
        agent.record_in_store(&tool_msg);
    }
    Ok(())
}

/// Run the round's text-embedded tool calls (non-native models) and record
/// the results (request list + shared store).
///
/// Returns `true` when at least one call was executed — the caller must then
/// `continue` the LLM loop.
pub(crate) async fn run_text_embedded_calls(
    agent: &Agent,
    tool_defs: &Option<Vec<ToolDefinition>>,
    display_content: &str,
    cancel_token: &CancellationToken,
    messages: &mut Vec<Message>,
    tool_manager: &ToolManager,
) -> Result<bool, String> {
    if tool_defs.as_ref().map(|d| d.is_empty()).unwrap_or(false) {
        // No tools offered — skip text parsing entirely.
        return Ok(false);
    }
    let mut embedded = Vec::new();
    if let Some(bash_calls) = agent.extract_bash_as_tool_calls(display_content) {
        embedded.extend(bash_calls);
    }
    if embedded.is_empty() {
        if let Some(json_calls) = agent.parse_tool_calls(display_content) {
            embedded.extend(json_calls);
        }
    }
    if embedded.is_empty() {
        return Ok(false);
    }
    for call in &embedded {
        if cancel_token.is_cancelled() {
            return Err("Cancelled".to_string());
        }
        agent.send_event(crate::types::AppEvent::ToolCallStart {
            tool_name: call.function.name.clone(),
            call_id: call.id.clone(),
            args_preview: crate::tools::tool_args_summary(
                &call.function.name,
                &call.function.arguments,
            ),
            session_id: agent.session_id(),
        });
        let params = match crate::tools::manager::parse_tool_args(&call.function.arguments) {
            Ok(p) => p,
            Err(e) => {
                agent.send_event(crate::types::AppEvent::ToolCallError {
                    tool_name: call.function.name.clone(),
                    call_id: call.id.clone(),
                    error: e.clone(),
                    session_id: agent.session_id(),
                });
                continue;
            }
        };
        let manager = tool_manager.clone();
        let progress = agent.tool_progress_for(&call.function.name, &call.id);
        let tool_result = manager
            .execute_with_progress(&call.function.name, params, &progress)
            .await;
        let result_str = match tool_result {
            Ok(output) => {
                let s = format!("{}", output);
                if s.trim().is_empty() {
                    "(no output)".to_string()
                } else {
                    s
                }
            }
            Err(e) => format!("Error: {}", e),
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
                call_type: call._call_type.clone(),
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
        let fb_tool = Message {
            role: "tool".to_string(),
            content: msg_content,
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: Some(call.id.clone()),
            reasoning_content: None,
            image: msg_image,
        };
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
