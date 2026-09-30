use super::*;

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

// ── P5: early tool-ready detection must not re-scan/re-clone growing args ──

/// Build a `data: {json}\n` SSE line. `serde_json::json!` keeps the escaping
/// self-validating (a malformed line would panic here instead of being
/// silently skipped downstream as an unparseable data line).
fn sse_line(v: serde_json::Value) -> String {
    format!("data: {}\n", v)
}

fn tool_call_delta_line(id: Option<&str>, index: u64, args: &str) -> String {
    let mut tc = serde_json::json!({ "index": index, "function": { "arguments": args } });
    if let Some(id) = id {
        // The FIRST chunk of a tool call carries id + name (later
        // continuations carry index + arguments only).
        tc["id"] = serde_json::Value::String(id.to_string());
        tc["function"]["name"] = serde_json::Value::String("write_file".to_string());
    }
    sse_line(serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [tc] } }],
    }))
}

fn thinking_delta_line(text: &str) -> String {
    sse_line(serde_json::json!({
        "choices": [{ "delta": { "reasoning": text } }],
    }))
}

/// Regression test (P5): for a multi-delta tool call with interleaved
/// thinking deltas, the ready callback fires EXACTLY ONCE, with the fully
/// accumulated arguments — when the args become complete (a thinking delta
/// after the last arg delta signals the model moved past the call), and no
/// further times on later deltas.
#[tokio::test]
async fn test_tool_ready_fires_exactly_once() {
    let client = ChatClient::new("http://localhost:8080");
    let conv = client.conversation();
    {
        let mut c = conv.lock().unwrap();
        c.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
    }

    let mut ready: Vec<ToolCall> = Vec::new();
    let mut tracker = ToolCallTracker::default();
    let mut noop_cb = |_: String, _: bool| -> Result<(), Error> { Ok(()) };
    let mut noop_pp = |_p: crate::types::PromptProgress| {};

    // 1) first tool-call delta: id + name + partial args (incomplete JSON)
    process_sse_line(
        &tool_call_delta_line(Some("call_1"), 0, r#"{"path": "a.txt", "content": "hel"#),
        &mut noop_cb,
        &conv,
        &mut |tc: ToolCall| ready.push(tc),
        &mut noop_pp,
        &mut tracker,
    )
    .await
    .unwrap();
    assert_eq!(ready.len(), 0, "args still incomplete");
    // 2) thinking delta → ready check runs, args still incomplete
    process_sse_line(
        &thinking_delta_line("hmm"),
        &mut noop_cb,
        &conv,
        &mut |tc: ToolCall| ready.push(tc),
        &mut noop_pp,
        &mut tracker,
    )
    .await
    .unwrap();
    assert_eq!(ready.len(), 0, "args still incomplete");
    // 3) continuation arg delta (index only) — args now complete
    //    (`"}` closes the string AND the object)
    process_sse_line(
        &tool_call_delta_line(None, 0, "lo\"}"),
        &mut noop_cb,
        &conv,
        &mut |tc: ToolCall| ready.push(tc),
        &mut noop_pp,
        &mut tracker,
    )
    .await
    .unwrap();
    // 4) thinking delta → model moved past the call → fires exactly once
    process_sse_line(
        &thinking_delta_line("."),
        &mut noop_cb,
        &conv,
        &mut |tc: ToolCall| ready.push(tc),
        &mut noop_pp,
        &mut tracker,
    )
    .await
    .unwrap();
    assert_eq!(ready.len(), 1, "ready must fire when args complete");
    // 5) further deltas → must NOT fire again
    process_sse_line(
        &thinking_delta_line("."),
        &mut noop_cb,
        &conv,
        &mut |tc: ToolCall| ready.push(tc),
        &mut noop_pp,
        &mut tracker,
    )
    .await
    .unwrap();
    assert_eq!(ready.len(), 1, "ready must fire exactly once per id");

    assert_eq!(ready[0].id, "call_1");
    assert_eq!(ready[0].function.name, "write_file");
    assert_eq!(
        ready[0].function.arguments,
        r#"{"path": "a.txt", "content": "hello"}"#
    );
    // The conversation must hold the same fully accumulated arguments.
    let c = conv.lock().unwrap();
    let tc = c
        .last()
        .and_then(|m| m.tool_calls.as_ref())
        .and_then(|tcs| tcs.iter().find(|t| t.id == "call_1"))
        .expect("call_1 accumulated in the conversation");
    assert_eq!(
        tc.function.arguments,
        r#"{"path": "a.txt", "content": "hello"}"#
    );
    drop(c);
}

/// Timing guard (P5): before the `checked_len` cache, EVERY thinking delta
/// re-cloned and re-scanned the entire (large) arguments string of the
/// active tool call — O(args²) total. With a ~1 MB arg payload that never
/// completes and 200 interleaved thinking deltas, the post-fix path is 1
/// scan + 199 length comparisons (milliseconds); a regression to
/// clone+scan-per-delta costs ~400 MB of copying/scanning and blows the
/// bound even in a debug build.
#[tokio::test]
async fn test_tool_ready_large_args_interleaved_thinking_is_linear() {
    let client = ChatClient::new("http://localhost:8080");
    let conv = client.conversation();
    {
        let mut c = conv.lock().unwrap();
        c.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
    }

    // Incomplete args (no closing brace) that never complete → the call is
    // never ready, so every thinking delta below re-runs the ready check
    // for the active id (the pre-fix clone+scan hot path).
    let big = "x".repeat(1_000_000); // ~1 MB of arguments payload
    let first = tool_call_delta_line(Some("call_big"), 0, &format!("{{\"content\": \"{}\"", big));
    let thinking = thinking_delta_line(".");

    let mut ready = 0usize;
    let mut tracker = ToolCallTracker::default();
    let mut noop_cb = |_: String, _: bool| -> Result<(), Error> { Ok(()) };
    let mut noop_pp = |_p: crate::types::PromptProgress| {};
    process_sse_line(
        &first,
        &mut noop_cb,
        &conv,
        &mut |_: ToolCall| ready += 1,
        &mut noop_pp,
        &mut tracker,
    )
    .await
    .unwrap();

    let started = std::time::Instant::now();
    for _ in 0..200 {
        process_sse_line(
            &thinking,
            &mut noop_cb,
            &conv,
            &mut |_: ToolCall| ready += 1,
            &mut noop_pp,
            &mut tracker,
        )
        .await
        .unwrap();
    }
    let elapsed = started.elapsed();
    assert_eq!(ready, 0, "args never complete → never ready");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "200 thinking deltas over a 1 MB active tool call took {:?} — \
         the per-delta args clone+scan (O(args²)) may have regressed",
        elapsed
    );
}
