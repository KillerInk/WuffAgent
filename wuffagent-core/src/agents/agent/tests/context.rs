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

    // No tools (schema_chars = 0), no images: zero overhead.
    assert_eq!(super::r#loop::request_overhead_chars(0, &msgs), 0);

    // Each attached image adds the fixed allowance (NOT the payload size).
    msgs[0].image = Some("data:image/png;base64,QUJD".to_string());
    assert_eq!(
        super::r#loop::request_overhead_chars(0, &msgs),
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
    // The cached schema count is the serialized JSON length (the value the
    // run computes ONCE up front and reuses every round).
    assert_eq!(super::r#loop::tool_schema_chars(Some(&defs)), expected_schema);
    assert_eq!(super::r#loop::tool_schema_chars(None), 0);
    assert_eq!(
        super::r#loop::request_overhead_chars(expected_schema, &msgs),
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
    let mut agent = Agent::builder(
        AgentConfig {
            name: "test".to_string(),
            ..Default::default()
        },
        Arc::new(NoopLlm),
        client,
    )
    .tool_manager(Arc::new(ToolManager::new(Arc::new(
        ToolRegistry::new(vec![], Arc::new(TracingToolLogger)),
    ))))
    .build();
    let mut messages = agent.build_initial_messages("write the file");
    let outcome = agent
        .run_llm_loop(&mut messages, &CancellationToken::new())
        .await
        .expect("run_llm_loop must complete");
    match outcome {
        super::r#loop::RunOutcome::Completed(content) => assert_eq!(content, "Done."),
        super::r#loop::RunOutcome::Handoff(_) => panic!("unexpected handoff outcome"),
        super::r#loop::RunOutcome::Restart(_) => panic!("unexpected restart outcome"),
        super::r#loop::RunOutcome::HandBack(_) => panic!("unexpected hand-back outcome"),
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

/// KV-prefix cache reuse: the query-aware memory block must NOT sit inside
/// the system prompt (message 0) — it is a separate system message right
/// after the turn's user message, so the prefix up to the new user message
/// is byte-identical between turns and llama.cpp's LCP slot matching can
/// reuse the KV cache (stop → continue must not re-prompt the whole
/// conversation).
#[test]
fn test_memory_block_sits_after_user_message_not_in_system_prompt() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-memory-placement-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let memory = crate::memory::MemoryManager::new(crate::memory::MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        injection_mode: crate::memory::InjectionMode::Always,
        ..Default::default()
    })
    .unwrap();
    let _ = memory.add(crate::memory::MemoryEntry::new(
        MemoryType::Fact,
        "The widget test fixture lives in fixtures/widget.json",
        "test",
        &[],
    ));

    let client = Arc::new(ChatClient::new("http://127.0.0.1:1"));
    let mut agent = Agent::builder(
        AgentConfig {
            name: "test".to_string(),
            ..Default::default()
        },
        Arc::new(NoopLlm),
        client.clone(),
    )
    .tool_manager(Arc::new(ToolManager::new(Arc::new(
        ToolRegistry::new(vec![], Arc::new(TracingToolLogger)),
    ))))
    .memory(Some(Arc::new(memory)))
    .build();

    // Turn 1: the user message is in the store (as `execute` does).
    let task1 = "fix the widget test";
    agent.client.conversation().lock().unwrap().push(Message {
        role: "user".to_string(),
        content: task1.to_string(),
        timestamp: crate::types::format_timestamp(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });
    let msgs1 = agent.build_initial_messages(task1);

    // The system prompt itself carries no memory context.
    assert_eq!(msgs1[0].role, "system");
    assert!(
        !msgs1[0].content.contains("MEMORY CONTEXT"),
        "memory block must not be in the system prompt"
    );
    // The block is its own message, directly after the user's task message.
    let u = msgs1
        .iter()
        .position(|m| m.content == task1)
        .expect("the user message must be in the request");
    assert_eq!(msgs1[u + 1].role, "system");
    assert!(
        msgs1[u + 1]
            .content
            .contains("The widget test fixture lives in fixtures/widget.json"),
        "memory block must sit right after the user message: {:?}",
        msgs1[u + 1].content
    );

    // Turn 2 (stop → continue): the prefix up to the new user message must
    // be byte-identical to turn 1's request — that is what the KV cache
    // reuses.
    let task2 = "now run the widget tests";
    agent.client.conversation().lock().unwrap().push(Message {
        role: "user".to_string(),
        content: task2.to_string(),
        timestamp: crate::types::format_timestamp(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });
    let msgs2 = agent.build_initial_messages(task2);
    let u2 = msgs2
        .iter()
        .position(|m| m.content == task2)
        .expect("turn-2 user message must be in the request");
    // The prefix through turn 1's user message is byte-identical; the
    // divergence starts exactly where turn 1's request-only memory block sat
    // (it was query-specific to task1 and is not re-sent). Everything the
    // store holds — the whole conversation — is cached.
    for (a, b) in msgs1.iter().take(u2).zip(msgs2.iter().take(u2)) {
        assert_eq!(a.role, b.role);
        assert_eq!(a.content, b.content, "prefix must be identical across turns");
    }
    assert_eq!(msgs2[u2 + 1].role, "system");
    assert!(
        msgs2[u2 + 1].content.contains("MEMORY CONTEXT"),
        "turn-2 memory block must sit right after the turn-2 user message"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
