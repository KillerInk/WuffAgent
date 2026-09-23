//! Unit tests for the `client` module (see `super`).

use super::*;
use crate::types::ToolCall;

#[test]
fn test_build_request_reasoning_effort() {
    let mut client = ChatClient::new("http://localhost:8080");

    // Off: `reasoning_effort` omitted, thinking explicitly disabled via the
    // Qwen3 chat-template kwarg (Qwen3.x defaults to thinking ON at xhigh and
    // has no off-level for `reasoning_effort`).
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
    assert_eq!(
        request.chat_template_kwargs,
        Some(http::ChatTemplateKwargs {
            enable_thinking: false
        })
    );
    let json = serde_json::to_string(&request).unwrap();
    assert!(!json.contains("reasoning_effort"));
    assert!(json.contains(r#""chat_template_kwargs":{"enable_thinking":false}"#));

    // High: serialized as "xhigh" (Qwen3 template wire value) with thinking
    // explicitly enabled.
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
    assert_eq!(
        request.chat_template_kwargs,
        Some(http::ChatTemplateKwargs {
            enable_thinking: true
        })
    );
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains(r#""reasoning_effort":"xhigh""#));
    assert!(json.contains(r#""chat_template_kwargs":{"enable_thinking":true}"#));
}

#[test]
fn test_reasoning_wire_all_levels() {
    use crate::types::ReasoningEffort;
    use http::ChatTemplateKwargs;

    // (effort, enable_thinking) per level — the single source of truth for
    // every request-construction site.
    let cases = [
        (ReasoningEffort::Off, None, false),
        (ReasoningEffort::Low, Some("low"), true),
        (ReasoningEffort::Medium, Some("medium"), true),
        (ReasoningEffort::High, Some("xhigh"), true),
    ];
    for (effort, wire, thinking) in cases {
        assert_eq!(effort.enable_thinking(), thinking, "enable_thinking({effort:?})");
        let (reasoning_effort, chat_template_kwargs) = http::reasoning_wire(effort);
        assert_eq!(reasoning_effort.as_deref(), wire, "reasoning_effort({effort:?})");
        assert_eq!(
            chat_template_kwargs,
            Some(ChatTemplateKwargs {
                enable_thinking: thinking
            }),
            "chat_template_kwargs({effort:?})"
        );
    }
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
        timings: None,
    }));
    assert_eq!(client.chars_per_token_x100(), 333);

    // CJK content: 500 chars -> 500 tokens => ratio 1.0 (clamped lower bound)
    client.note_prompt_chars(500);
    client.calibrate_from_usage(Some(&crate::types::Usage {
        prompt_tokens: 500,
        completion_tokens: 10,
        total_tokens: 510,
        timings: None,
    }));
    assert_eq!(client.chars_per_token_x100(), 100);

    // No usage / zero tokens: ratio unchanged
    client.note_prompt_chars(1000);
    client.calibrate_from_usage(None);
    client.calibrate_from_usage(Some(&crate::types::Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        timings: None,
    }));
    assert_eq!(client.chars_per_token_x100(), 100);

    // Glitched measurement (ratio 100.0) clamps to the 10.0 upper bound
    client.note_prompt_chars(1000);
    client.calibrate_from_usage(Some(&crate::types::Usage {
        prompt_tokens: 1,
        completion_tokens: 0,
        total_tokens: 1,
        timings: None,
    }));
    assert_eq!(client.chars_per_token_x100(), 1000);
}

#[test]
fn test_trim_trigger_and_target_chars_use_calibrated_ratio() {
    let client = ChatClient::new("http://localhost:8080");
    client.set_n_ctx(100_096);
    // Default 3.5 chars/token:
    // trigger: 100096 × 0.9 × 3.5 = 315292.8 → 315292
    assert_eq!(
        client.trim_trigger_chars(),
        (100_096u64 * 90 * 350 / 10_000) as usize
    );
    // target:  100096 × 0.5 × 3.5 = 175168.0 → 175168
    assert_eq!(
        client.trim_target_chars(),
        (100_096u64 * 50 * 350 / 10_000) as usize
    );

    // Calibrate to 3.0 chars/token (English prose): both budgets shrink
    client.note_prompt_chars(30_000);
    client.calibrate_from_usage(Some(&crate::types::Usage {
        prompt_tokens: 10_000,
        completion_tokens: 0,
        total_tokens: 10_000,
        timings: None,
    }));
    assert_eq!(
        client.trim_trigger_chars(),
        (100_096u64 * 90 * 300 / 10_000) as usize
    );
    assert_eq!(
        client.trim_target_chars(),
        (100_096u64 * 50 * 300 / 10_000) as usize
    );

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
        timings: None,
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
        "Server returned 400 Bad Request: {\"error\":{\"message\":\"bad request\"}}".to_string(),
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
    assert_eq!(
        parse_props_n_ctx(r#"{"default_generation_settings":{}}"#),
        None
    );
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
    let tools = vec![crate::tools::ToolDefinition {
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
    }];
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
        conv.push(Message {
            role: "user".into(),
            content: "Hi there".into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
        conv.push(Message {
            role: "assistant".into(),
            content: "Hello! How can I help?".into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
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
        conv.push(Message {
            role: "user".into(),
            content: "Hi".into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
        conv.push(Message {
            role: "assistant".into(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
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
    let client = ChatClient::new("http://localhost:8080");
    assert_eq!(client.api_key(), None);
    client.set_api_key(Some("sk-test"));
    assert_eq!(client.api_key(), Some("sk-test".to_string()));
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
    let result = process_sse_line(
        "",
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
    let result = process_sse_line(
        "data: [DONE]",
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
    assert!(result.is_ok());
    assert!(captured.is_empty());
}

#[tokio::test]
async fn test_process_sse_line_prompt_progress() {
    use crate::types::PromptProgress;
    let client = ChatClient::new("http://localhost:8080");
    let mut cb = |_s: String, _t: bool| -> Result<(), Error> { Ok(()) };
    let mut seen: Vec<PromptProgress> = Vec::new();
    let mut pp_cb = |pp: PromptProgress| {
        seen.push(pp);
    };
    // llama.cpp progress chunk: `prompt_progress` is a sibling of `choices`,
    // the delta carries no content (content: null).
    let sse = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null}}],\"prompt_progress\":{\"total\":1000,\"cache\":200,\"processed\":500,\"time_ms\":1200.5}}\n";
    let result = process_sse_line(
        sse,
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut pp_cb,
        &mut ToolCallTracker::default(),
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(seen.len(), 1);
    let pp = seen[0];
    assert_eq!(pp.total, 1000);
    assert_eq!(pp.cache, 200);
    assert_eq!(pp.processed, 500);
    // speed counts only NON-cached tokens: (500 - 200) / 1.2005 s
    assert!((pp.prompt_tps().unwrap() - 300.0 / 1.2005).abs() < 1e-9);
}

#[test]
fn test_prompt_tps_guards() {
    use crate::types::PromptProgress;
    // Initial 0% chunk: everything "processed" is cached, no measurable time.
    assert_eq!(
        PromptProgress {
            total: 100,
            cache: 100,
            processed: 100,
            time_ms: 0.0
        }
        .prompt_tps(),
        None
    );
    // Sub-millisecond: skip to avoid a noisy speed spike.
    assert_eq!(
        PromptProgress {
            total: 100,
            cache: 0,
            processed: 10,
            time_ms: 0.4
        }
        .prompt_tps(),
        None
    );
}

#[test]
fn test_build_request_return_progress() {
    let client = ChatClient::new("http://localhost:8080");
    let stream_req = build_request(
        &client.system_prompt,
        &client.conversation(),
        "Hello",
        true,
        None,
        client.reasoning_effort(),
        4096,
    );
    assert_eq!(stream_req.return_progress, Some(true));
    let non_stream_req = build_request(
        &client.system_prompt,
        &client.conversation(),
        "Hello",
        false,
        None,
        client.reasoning_effort(),
        4096,
    );
    assert_eq!(non_stream_req.return_progress, None);
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
    let result = process_sse_line(
        "data: [DONE]\n",
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
    let result = process_sse_line(
        "data: [DONE]\r\n",
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
        image: None,
    });
    drop(conv);

    let sse_data = r#"data:{"choices":[{"delta":{"content":"Hi"}}]}"#;
    let mut captured = Vec::new();
    let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
        captured.push(s);
        Ok(())
    };
    let result = process_sse_line(
        sse_data,
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
    let result = process_sse_line(
        "id: 1",
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
        image: None,
    });
    drop(conv);

    let sse_data = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
    let mut captured = Vec::new();
    let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
        captured.push(s);
        Ok(())
    };
    let result = process_sse_line(
        sse_data,
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
        image: None,
    });
    drop(conv);

    let sse1 = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
    let mut captured = Vec::new();
    let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
        captured.push(s);
        Ok(())
    };
    process_sse_line(
        sse1,
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await
    .unwrap();

    let sse2 = r#"data: {"choices":[{"delta":{"content":" world"}}]}"#;
    let mut captured2 = Vec::new();
    let mut cb2 = |s: String, _is_thinking: bool| -> Result<(), Error> {
        captured2.push(s);
        Ok(())
    };
    process_sse_line(
        sse2,
        &mut cb2,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await
    .unwrap();

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
        image: None,
    });
    drop(conv);

    let sse_data = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
    let mut captured = Vec::new();
    let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
        captured.push(s);
        Ok(())
    };
    let result = process_sse_line(
        sse_data,
        &mut cb,
        &client.conversation(),
        &mut |_tc: ToolCall| {},
        &mut |_| {},
        &mut ToolCallTracker::default(),
    )
    .await;
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
        image: None,
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
        image: None,
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
        image: None,
    });
    drop(conv);

    let warnings = client.check_tool_call_warnings();
    assert!(warnings.is_empty());
}

// ── Early tool-call ("ready") detection ─────────────────────────────
// A tool call becomes ready to execute the moment the stream moves past
// it (a text delta or the next tool call) and its arguments look like
// complete JSON.

fn seed_empty_assistant(client: &ChatClient) {
    let mut conv = client.conversation().lock().unwrap();
    conv.push(Message {
        role: "assistant".to_string(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });
}

/// Shared sink for ready callbacks so tests can inspect reported calls
/// without holding a borrow across the callback.
type ReadySink = std::sync::Arc<std::sync::Mutex<Vec<ToolCall>>>;

fn make_ready_sink() -> (ReadySink, impl FnMut(ToolCall)) {
    let sink: ReadySink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let inner = std::sync::Arc::clone(&sink);
    (sink, move |tc: ToolCall| {
        inner.lock().unwrap().push(tc);
    })
}

#[tokio::test]
async fn test_ready_fires_when_model_moves_past_tool_call() {
    let client = ChatClient::new("http://localhost:8080");
    seed_empty_assistant(&client);
    let (ready, mut ready_cb) = make_ready_sink();
    let mut tracker = ToolCallTracker::default();
    let mut cb = |_s: String, _t: bool| -> Result<(), Error> { Ok(()) };

    // First line: a tool call with complete JSON arguments.
    let tc_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc1","function":{"name":"shell","arguments":"{\"command\":\"ls\"}"}}]}}]}"#;
    process_sse_line(
        tc_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    assert!(
        ready.lock().unwrap().is_empty(),
        "no signal yet: the stream is still inside the tool call"
    );

    // Second line: a text delta — the model moved past the tool call.
    let text_line = r#"data: {"choices":[{"delta":{"content":"here you go"}}]}"#;
    process_sse_line(
        text_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    let reported = ready.lock().unwrap();
    assert_eq!(reported.len(), 1, "moving past the call must report it");
    assert_eq!(reported[0].id, "tc1");
    assert_eq!(reported[0].function.name, "shell");
    assert_eq!(reported[0].function.arguments, r#"{"command":"ls"}"#);
    drop(reported);

    // A further text delta must not report the same call again.
    process_sse_line(
        text_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    assert_eq!(
        ready.lock().unwrap().len(),
        1,
        "a call is reported exactly once"
    );
}

#[tokio::test]
async fn test_ready_waits_for_complete_args() {
    let client = ChatClient::new("http://localhost:8080");
    seed_empty_assistant(&client);
    let (ready, mut ready_cb) = make_ready_sink();
    let mut tracker = ToolCallTracker::default();
    let mut cb = |_s: String, _t: bool| -> Result<(), Error> { Ok(()) };

    // Partial arguments — the object is not closed yet.
    let part_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc1","function":{"name":"shell","arguments":"{\"command\": \"l"}}]}}]}"#;
    process_sse_line(
        part_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    let text_line = r#"data: {"choices":[{"delta":{"content":"x"}}]}"#;
    process_sse_line(
        text_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    assert!(
        ready.lock().unwrap().is_empty(),
        "args are still incomplete JSON"
    );

    // Continuation chunk (index only) completes the arguments...
    let cont_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"s\"}"}}]}}]}"#;
    process_sse_line(
        cont_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    assert!(
        ready.lock().unwrap().is_empty(),
        "still inside the tool call"
    );

    // ...and the next text delta reports the fully accumulated call.
    process_sse_line(
        text_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    let reported = ready.lock().unwrap();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].id, "tc1");
    assert_eq!(reported[0].function.arguments, r#"{"command": "ls"}"#);
}

#[tokio::test]
async fn test_ready_fires_for_prior_call_when_next_starts() {
    let client = ChatClient::new("http://localhost:8080");
    seed_empty_assistant(&client);
    let (ready, mut ready_cb) = make_ready_sink();
    let mut tracker = ToolCallTracker::default();
    let mut cb = |_s: String, _t: bool| -> Result<(), Error> { Ok(()) };

    let tc1_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc1","function":{"name":"shell","arguments":"{\"command\":\"ls\"}"}}]}}]}"#;
    process_sse_line(
        tc1_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    // tc2 starts: tc1 is reported, tc2 is not (it is still streaming).
    let tc2_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"tc2","function":{"name":"read_file","arguments":"{\"path\": \"a.txt\"}"}}]}}]}"#;
    process_sse_line(
        tc2_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    let reported = ready.lock().unwrap();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].id, "tc1");
    drop(reported);

    // Text after tc2: tc2 is reported too.
    let text_line = r#"data: {"choices":[{"delta":{"content":"done"}}]}"#;
    process_sse_line(
        text_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    let reported = ready.lock().unwrap();
    assert_eq!(reported.len(), 2);
    assert_eq!(reported[1].id, "tc2");
}

#[test]
fn test_looks_like_complete_json() {
    assert!(looks_like_complete_json(r#"{"a": 1}"#));
    assert!(looks_like_complete_json(r#"{"a": {"b": [1, 2]}}"#));
    assert!(looks_like_complete_json(r#"{"s": "a}b{"}"#));
    assert!(looks_like_complete_json(" {} "));
    assert!(!looks_like_complete_json(r#"{"a": 1"#));
    assert!(!looks_like_complete_json(r#"{"s": "unterminated""#));
    assert!(!looks_like_complete_json(""));
    assert!(!looks_like_complete_json("not json"));
}

/// End-to-end choke-point test: one completed (non-stream) LLM call against a
/// mock OpenAI-compatible server appends exactly one usage.jsonl line with the
/// server-reported usage, the response's model name, and the agent stamp.
#[tokio::test]
async fn test_completed_call_logs_one_usage_line() {
    use std::io::{Read, Write};

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("usage.jsonl");

    // Mock server: one TCP connection, read the request head, reply with a
    // canned chat completion, close.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let mut head = Vec::new();
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // The mock assistant issues two tool calls and some reasoning text,
        // so the usage line must carry tool_calls=2 and the thinking length.
        let thinking = "thinking hard";
        let body: Vec<u8> = format!(
            r#"{{"model":"mock-model","choices":[{{"message":{{"role":"assistant","content":"hello back","reasoning_content":"{thinking}","tool_calls":[{{"id":"tc1","type":"function","function":{{"name":"shell","arguments":"{{}}"}}}},{{"id":"tc2","type":"function","function":{{"name":"read_file","arguments":"{{}}"}}}}]}},"finish_reason":"tool_calls"}}],"usage":{{"prompt_tokens":120,"completion_tokens":34,"total_tokens":154}},"timings":{{"prompt_n":120,"prompt_ms":972.1,"prompt_per_second":123.45,"predicted_n":34,"predicted_ms":501.5,"predicted_per_second":67.89}}}}"#
        )
        .into_bytes();
        let resp_head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(resp_head.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    });

    let mut client = ChatClient::new(&format!("http://127.0.0.1:{port}/chat/completions"));
    client.set_usage_recorder(std::sync::Arc::new(
        crate::usage::recorder::UsageRecorder::new(log_path.clone()),
    ));
    client.set_agent_name("coder");

    let (content, usage) = client.send_message("hi").await.unwrap();
    server.join().unwrap();

    assert_eq!(content, "hello back");
    let u = usage.as_ref().unwrap();
    assert_eq!(u.total_tokens, 154);
    // llama.cpp-style `timings` (sibling of `usage`) must be folded in.
    let t = u.timings.as_ref().unwrap();
    assert!((t.prompt_per_second.unwrap() - 123.45).abs() < 1e-9);
    assert!((t.predicted_per_second.unwrap() - 67.89).abs() < 1e-9);

    let (entries, skipped) = crate::usage::stats::load_entries(&log_path);
    assert_eq!(skipped, 0);
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.agent, "coder");
    assert_eq!(e.model, "mock-model");
    assert_eq!(e.prompt_tokens, 120);
    assert_eq!(e.completion_tokens, 34);
    assert_eq!(e.total_tokens, 154);
    assert_eq!(e.tool_calls, 2);
    assert_eq!(e.thinking_chars, "thinking hard".chars().count() as u64);
    // The line must be valid standalone JSONL with a UTC timestamp.
    let raw = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(r#""ts":"#));
    assert!(lines[0].ends_with('}'));
}
