use super::*;

/// The nudge is request-only, but a user message that merely repeats the
/// nudge text (with a real timestamp) must stay storable.
#[test]
fn test_is_storable_nudge_vs_user_typed_nudge() {
    // The verification nudge as the loop pushes it: empty timestamp.
    let nudge = Message {
        role: "user".to_string(),
        content: VERIFICATION_NUDGE.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    };
    assert!(!Agent::is_storable(&nudge));

    // A human typing the same sentence: real timestamp, stays in the store.
    let typed = Message {
        role: "user".to_string(),
        content: VERIFICATION_NUDGE.to_string(),
        timestamp: crate::types::format_timestamp(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    };
    assert!(Agent::is_storable(&typed));
}

/// The trim budget must reserve the per-request overhead (tool schemas +
/// images) that `message_char_count` does not count, or the request can
/// exceed n_ctx while the messages alone are under the trim target.
#[test]
fn test_request_overhead_counts_tool_schemas_and_images() {
    let mut msgs = vec![Message {
        role: "user".to_string(),
        content: "hi".to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }];

    // No tools, no images: zero overhead.
    assert_eq!(super::r#loop::request_overhead_chars(None, &msgs), 0);

    // Each attached image adds the fixed allowance (NOT the payload size).
    msgs[0].image = Some("data:image/png;base64,QUJD".to_string());
    assert_eq!(
        super::r#loop::request_overhead_chars(None, &msgs),
        super::r#loop::ESTIMATED_IMAGE_CHARS
    );

    // Tool schemas add their serialized JSON length, on top of the image.
    let defs: Vec<crate::tools::ToolDefinition> = (0..3)
        .map(|i| crate::tools::ToolDefinition {
            type_name: "function".to_string(),
            function: crate::tools::ToolFunctionSpec {
                name: format!("tool_{i}"),
                description: "does a thing".to_string(),
                parameters: crate::tools::JsonSchema {
                    type_name: "object".to_string(),
                    properties: None,
                    required: vec![],
                },
            },
        })
        .collect();
    let expected_schema = serde_json::to_string(&defs).unwrap().chars().count();
    assert_eq!(
        super::r#loop::request_overhead_chars(Some(&defs), &msgs),
        expected_schema + super::r#loop::ESTIMATED_IMAGE_CHARS
    );
    assert!(
        expected_schema > 100,
        "sanity: three tool schemas must serialize to more than 100 chars"
    );
}

/// Regression test: a tool call whose arguments were truncated by the
/// model's output limit (the stream ended mid-JSON) must never be stored
/// raw. The shared store (and with it the session file and every future LLM
/// request) must hold the repaired arguments, and the model must receive a
/// tool result explaining that the call did not execute.
#[tokio::test]
async fn test_truncated_tool_call_repaired_before_storing() {
    // Canned SSE server:
    //   request 1 → one tool call whose arguments end mid-string;
    //   request 2 → the model's final text response.
    let tool_chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_trunc1",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"content\":\"use super::state"
                    }
                }]
            }
        }]
    });
    let body1 = format!("data: {tool_chunk}\n\ndata: [DONE]\n\n");
    let text_chunk =
        serde_json::json!({"choices": [{"delta": {"role": "assistant", "content": "Done."}}]});
    let body2 = format!("data: {text_chunk}\n\ndata: [DONE]\n\n");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut requests = 0usize;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            // Read the request: header block, then the Content-Length body.
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 8192];
            let mut header_end: Option<usize> = None;
            while header_end.is_none() {
                let n = match stream.read(&mut tmp) {
                    Ok(n) if n > 0 => n,
                    _ => break,
                };
                buf.extend_from_slice(&tmp[..n]);
                header_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
            }
            let Some(pos) = header_end else { break };
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            let content_length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            // The header read may have swallowed body bytes into `buf`
            // already — count those so we don't block on a read that will
            // never come.
            let mut have = buf.len().saturating_sub(pos + 4);
            while have < content_length {
                let n = match stream.read(&mut tmp) {
                    Ok(n) if n > 0 => n,
                    _ => break,
                };
                have += n;
            }
            requests += 1;
            let body = if requests == 1 { body1.clone() } else { body2.clone() };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            if requests >= 2 {
                break;
            }
        }
    });

    let client = Arc::new(ChatClient::new(&format!("http://{addr}")));
    let mut agent = Agent::new(
        AgentConfig {
            name: "test".to_string(),
            ..Default::default()
        },
        Arc::new(NoopLlm),
        Arc::new(Mutex::new(ToolManager::new(Arc::new(
            ToolRegistry::new(vec![], Arc::new(TracingToolLogger)),
        )))),
        None,
        client,
        None,
        None,
    );
    let mut messages = agent.build_initial_messages("write the file");
    let outcome = agent
        .run_llm_loop(&mut messages, &CancellationToken::new())
        .await
        .expect("run_llm_loop must complete");
    match outcome {
        super::r#loop::RunOutcome::Completed(content) => assert_eq!(content, "Done."),
        super::r#loop::RunOutcome::Handoff(_) => panic!("unexpected handoff outcome"),
        super::r#loop::RunOutcome::Restart(_) => panic!("unexpected restart outcome"),
    }

    // Invariant: the shared store never holds incomplete tool call arguments.
    let store = agent.client.conversation().lock().unwrap();
    let mut saw_repaired = false;
    for m in store.iter() {
        for c in m.tool_calls.iter().flatten() {
            assert!(
                crate::tools::manager::tool_args_complete(&c.function.arguments),
                "stored tool call arguments must be complete JSON: {:?}",
                &c.function.arguments[..c.function.arguments.len().min(60)]
            );
            if c.id == "call_trunc1" {
                saw_repaired = true;
                assert_eq!(c.function.arguments, "{}");
            }
        }
    }
    assert!(
        saw_repaired,
        "the truncated call must be stored (repaired to {{}})"
    );
    // The model is told why the tool did not execute, so it can retry.
    let tool_msg = store
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("call_trunc1"))
        .expect("a tool result for the truncated call must be stored");
    assert!(
        tool_msg.content.contains("truncated"),
        "tool result must explain the truncation: {}",
        tool_msg.content
    );
}
