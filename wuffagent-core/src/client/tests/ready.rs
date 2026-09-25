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

#[test]
fn test_looks_like_complete_json_escape_and_nesting_edge_cases() {
    // Escaped quotes inside strings must not flip the in-string state.
    assert!(looks_like_complete_json(r#"{"cmd": "echo \"hi\""}"#));
    // A brace escaped INSIDE a string is just a string character.
    assert!(looks_like_complete_json(r#"{"s": "x}"}"#));
    // Deeply nested objects/arrays stay balanced.
    assert!(looks_like_complete_json(r#"{"a": [{"b": [[1]]}]}"#));
    // Trailing content after the closing quote still leaves a string open.
    assert!(!looks_like_complete_json(r#"{"a": "b}"#));
    // More closing than opening braces.
    assert!(!looks_like_complete_json(r#"{"a": 1}}"#));
    // Array top-level is not an object.
    assert!(!looks_like_complete_json("[1, 2]"));
    // A string that swallows the final brace.
    assert!(!looks_like_complete_json(r#""{"a": 1}"#));
}

#[tokio::test]
async fn test_ready_interleaved_continuations_land_in_right_call() {
    // tc1 streams, tc2 starts (reports tc1), then an INDEX-ONLY continuation
    // chunk for tc1 arrives while the stream is on tc2. The continuation must
    // be appended to tc1's accumulated arguments, not tc2's.
    let client = ChatClient::new("http://localhost:8080");
    seed_empty_assistant(&client);
    let (ready, mut ready_cb) = make_ready_sink();
    let mut tracker = ToolCallTracker::default();
    let mut cb = |_s: String, _t: bool| -> Result<(), Error> { Ok(()) };

    let tc1_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc1","function":{"name":"shell","arguments":"{\"command\": \"l"}}]}}]}"#;
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

    // tc2 starts: tc1 would be reported here, but its arguments are still
    // incomplete JSON — nothing fires yet.
    let tc2_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"tc2","function":{"name":"read_file","arguments":"{\"path\": \"a\"}"}}]}}]}"#;
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
    assert!(
        ready.lock().unwrap().is_empty(),
        "tc1's args are incomplete, so moving to tc2 must not report it"
    );

    // Index-only continuation for tc1 (slot 0) — no id in the chunk.
    let cont_tc1_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"s\"}"}}]}}]}"#;
    process_sse_line(
        cont_tc1_line,
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();

    // Text ends the stream: tc2 (still active) is reported.
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

    let conv = client.conversation().lock().unwrap();
    let tcs = conv
        .last()
        .expect("assistant message")
        .tool_calls
        .as_ref()
        .expect("two tool calls");
    assert_eq!(tcs.len(), 2);
    // tc1 got its continuation appended.
    assert_eq!(tcs[0].id, "tc1");
    assert_eq!(tcs[0].function.arguments, r#"{"command": "ls"}"#);
    // tc2 is untouched by tc1's continuation chunk.
    assert_eq!(tcs[1].id, "tc2");
    assert_eq!(tcs[1].function.arguments, r#"{"path": "a"}"#);
    drop(conv);

    let reported = ready.lock().unwrap();
    // tc2 was reported when the stream moved back to tc1 (its index-only
    // continuation); tc1 was reported by the final text delta.
    assert_eq!(reported.len(), 2);
    assert_eq!(reported[0].id, "tc2");
    assert_eq!(reported[1].id, "tc1");
    // tc1 was reported with the FULLY accumulated (interleaved) arguments.
    assert_eq!(reported[1].function.arguments, r#"{"command": "ls"}"#);
}

#[tokio::test]
async fn test_ready_last_call_never_fires_mid_stream() {
    // The very last tool call has nothing after it to signal completion: it
    // must never be reported (callers execute it inline after the stream).
    let client = ChatClient::new("http://localhost:8080");
    seed_empty_assistant(&client);
    let (ready, mut ready_cb) = make_ready_sink();
    let mut tracker = ToolCallTracker::default();
    let mut cb = |_s: String, _t: bool| -> Result<(), Error> { Ok(()) };

    let tc1_line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc1","function":{"name":"shell","arguments":"{\"command\": \"ls\"}"}}]}}]}"#;
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

    // Stream ends right after the tool call: [DONE] is not a "moved past"
    // signal, and a usage chunk is neither.
    process_sse_line(
        "data: [DONE]\n",
        &mut cb,
        &client.conversation(),
        &mut ready_cb,
        &mut |_| {},
        &mut tracker,
    )
    .await
    .unwrap();
    let usage_line = r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
    process_sse_line(
        usage_line,
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
        "the last tool call must not be reported mid-stream"
    );
}
