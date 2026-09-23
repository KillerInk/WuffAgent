use super::*;

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
