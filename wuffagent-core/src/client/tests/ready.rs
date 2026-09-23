use super::*;

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
