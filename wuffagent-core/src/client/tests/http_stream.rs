//! Live-server tests for the HTTP layer (`http.rs`): `send_message`
//! (non-streaming) and `sse::stream_message` (SSE streaming + cancellation).
//!
//! A hand-rolled one-shot HTTP/1.1 server on a local `tokio::net::TcpListener`
//! (no mock framework, no new dependency): the helper reads the request head,
//! then hands the socket to a per-test handler. The handlers write canned
//! responses — for the streaming tests the body is flushed in several pieces
//! with small pauses so the client's line buffer actually reassembles lines
//! split across TCP writes.

use std::future::Future;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::*;

/// Spawn a one-shot HTTP server on 127.0.0.1:0. The handler receives the
/// socket AFTER the request head (through "\r\n\r\n") has been read. Returns
/// the base URL and the task handle (abort/await it at test end).
async fn spawn_server<F, Fut>(handler: F) -> (String, tokio::task::JoinHandle<()>)
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (sock, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sock = sock;
        read_request_head(&mut sock).await;
        handler(sock).await;
    });
    (format!("http://{addr}"), handle)
}

/// Read until the end of the HTTP request head (or socket close / 1 MB cap).
/// The body is irrelevant to these tests.
async fn read_request_head(sock: &mut TcpStream) {
    let mut buf = [0u8; 4096];
    let mut data: Vec<u8> = Vec::new();
    loop {
        match sock.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if data.windows(4).any(|w| w == b"\r\n\r\n") || data.len() > 1_048_576 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// Write a fixed-length HTTP/1.1 response and let the socket close. The
/// response head is built up-front so the returned future owns everything
/// (`'static`, no captured lifetimes).
fn write_response(
    sock: TcpStream,
    status: &str,
    content_type: &str,
    body: Vec<u8>,
) -> impl Future<Output = ()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    async move {
        let mut sock = sock;
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(&body).await;
        let _ = sock.flush().await;
    }
}

/// A complete canned SSE body (one line per chunk) for the streaming tests.
const SSE_BODY: &str = concat!(
    "data: {\"model\":\"mock-7b\",\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n",
    ": keep-alive\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"lo \"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"thinking\":\"hmm\"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc1\",\"type\":\"function\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"now \"}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"tc2\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\" \\\"/a\\\"}\"}}]}}]}\n",
    "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n",
    "data: [DONE]\n"
);

#[tokio::test]
async fn test_send_message_success_parses_all_fields() {
    let body = r#"{
        "model": "mock-7b",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "done",
                "reasoning_content": "hmm hmm",
                "tool_calls": [{
                    "id": "tc1", "type": "function",
                    "function": {"name": "shell", "arguments": "{\"command\":\"ls\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10},
        "timings": {"predicted_per_second": 12.5}
    }"#;
    let (base, handle) = spawn_server(move |sock| {
        write_response(sock, "200 OK", "application/json", body.as_bytes().to_vec())
    })
    .await;

    let conv = Arc::new(std::sync::Mutex::new(Vec::new()));
    let request = build_request("sys", &conv, "hi", false, None, crate::types::ReasoningEffort::Off, 4096);
    let http = reqwest::Client::new();
    let result = send_message(&http, &base, Some("key"), &request)
        .await
        .expect("send_message");

    assert_eq!(result.content, "done");
    assert_eq!(result.model.as_deref(), Some("mock-7b"));
    assert_eq!(result.tool_calls, 1);
    assert_eq!(result.thinking_chars, 7);
    let usage = result.usage.expect("usage");
    assert_eq!(usage.total_tokens, 10);
    assert_eq!(
        usage.timings.as_ref().and_then(|t| t.predicted_per_second),
        Some(12.5),
        "timings must be folded into usage"
    );
    let _ = handle.await;
}

#[tokio::test]
async fn test_send_message_server_error_maps_to_http_error() {
    let (base, handle) = spawn_server(move |sock| {
        write_response(sock, "429 Too Many Requests", "text/plain", b"slow down".to_vec())
    })
    .await;

    let conv = Arc::new(std::sync::Mutex::new(Vec::new()));
    let request = build_request("", &conv, "hi", false, None, crate::types::ReasoningEffort::Off, 4096);
    let err = send_message(&reqwest::Client::new(), &base, None, &request)
        .await
        .unwrap_err();
    match err {
        Error::Http(msg) => {
            assert!(msg.contains("429"), "got: {msg}");
            assert!(msg.contains("slow down"), "got: {msg}");
        }
        other => panic!("expected Http error, got: {other:?}"),
    }
    let _ = handle.await;
}

#[tokio::test]
async fn test_send_message_invalid_json_maps_to_json_error() {
    let (base, handle) = spawn_server(move |sock| {
        write_response(sock, "200 OK", "application/json", b"not json".to_vec())
    })
    .await;

    let conv = Arc::new(std::sync::Mutex::new(Vec::new()));
    let request = build_request("", &conv, "hi", false, None, crate::types::ReasoningEffort::Off, 4096);
    match send_message(&reqwest::Client::new(), &base, None, &request).await.unwrap_err() {
        Error::Json(_) => {}
        other => panic!("expected Json error, got: {other:?}"),
    }
    let _ = handle.await;
}

#[tokio::test]
async fn test_stream_message_full_sse_accumulates_and_returns_usage() {
    // Write the SSE body in three pieces (split mid-line) with pauses, so
    // the client's line buffer must reassemble lines from separate writes.
    let body = SSE_BODY.as_bytes().to_vec();
    let (cut1, cut2) = (body.len() / 3, body.len() * 2 / 3);
    let (base, handle) = spawn_server(move |sock| async move {
        let mut sock = sock;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = sock.write_all(head.as_bytes()).await;
        for chunk in [&body[..cut1], &body[cut1..cut2], &body[cut2..]] {
            let _ = sock.write_all(chunk).await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    let conv = Arc::new(std::sync::Mutex::new(Vec::new()));
    add_streaming_messages(&conv, "hi");
    let request = build_request("sys", &conv, "hi", true, None, crate::types::ReasoningEffort::Off, 4096);
    let resp = build_stream_request(&reqwest::Client::new(), &base, None, &request)
        .send()
        .await
        .expect("stream request");

    // stream_message's callbacks are 'static, so capture state via Arcs.
    let text: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let thinking: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ready: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (t2, th2, r2) = (text.clone(), thinking.clone(), ready.clone());
    let mut tracker = ToolCallTracker::default();
    let (usage, model) = stream_message(
        resp,
        &conv,
        &mut move |chunk, is_thinking| {
            let v = if is_thinking { &th2 } else { &t2 };
            v.lock().unwrap().push(chunk);
            Ok(())
        },
        &mut move |tc: crate::types::ToolCall| {
            r2.lock().unwrap().push(tc.id);
        },
        &mut |_| {},
        &mut tracker,
        None,
    )
    .await
    .expect("stream_message");

    assert_eq!(text.lock().unwrap().concat(), "Hello now ");
    assert_eq!(thinking.lock().unwrap().concat(), "hmm");
    let last = conv.lock().unwrap().last().expect("assistant message").clone();
    assert_eq!(last.content, "Hello now ");
    assert_eq!(last.reasoning_content.as_deref(), Some("hmm"));
    let tcs = last.tool_calls.expect("tool calls").to_vec();
    assert_eq!(tcs.len(), 2);
    assert_eq!(tcs[0].id, "tc1");
    assert_eq!(tcs[0].function.arguments, r#"{"command":"ls"}"#);
    assert_eq!(tcs[1].id, "tc2");
    assert_eq!(tcs[1].function.arguments, r#"{"path": "/a"}"#);
    // Early ready-firing: tc1 completed mid-stream; tc2 (the last call)
    // never fires.
    let ready = ready.lock().unwrap().to_vec();
    assert_eq!(ready, vec!["tc1".to_string()]);
    assert_eq!(model.as_deref(), Some("mock-7b"));
    assert_eq!(usage.expect("usage").total_tokens, 15);
    let _ = handle.await;
}

#[tokio::test]
async fn test_stream_message_cancel_mid_stream() {
    let first = b"data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"tial\"}}]}\n";
    let rest = b"data: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}\ndata: [DONE]\n";
    let (base, handle) = spawn_server(move |sock| async move {
        let mut sock = sock;
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
        let _ = sock.write_all(head.as_bytes()).await;
        // First chunk (two content lines), then go silent for 2 s.
        let _ = sock.write_all(format!("{:x}\r\n", first.len()).as_bytes()).await;
        let _ = sock.write_all(first).await;
        let _ = sock.write_all(b"\r\n").await;
        let _ = sock.flush().await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        // Second chunk + final zero-length chunk (fails harmlessly if the
        // client already cancelled).
        let _ = sock.write_all(format!("{:x}\r\n", rest.len()).as_bytes()).await;
        let _ = sock.write_all(rest).await;
        let _ = sock.write_all(b"\r\n0\r\n\r\n").await;
        let _ = sock.flush().await;
    })
    .await;

    let conv = Arc::new(std::sync::Mutex::new(Vec::new()));
    add_streaming_messages(&conv, "hi");
    let request = build_request("sys", &conv, "hi", true, None, crate::types::ReasoningEffort::Off, 4096);
    let resp = build_stream_request(&reqwest::Client::new(), &base, None, &request)
        .send()
        .await
        .expect("stream request");

    let cancel = CancellationToken::new();
    let (cancel2, conv2) = (cancel.clone(), conv.clone());
    let stream_task = tokio::spawn(async move {
        let mut tracker = ToolCallTracker::default();
        stream_message(
            resp,
            &conv2,
            &mut |_chunk, _thinking| Ok(()),
            &mut |_tc| {},
            &mut |_| {},
            &mut tracker,
            Some(&cancel2),
        )
        .await
    });

    // Let the two content lines arrive, then cancel during the silence.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    cancel.cancel();
    match stream_task.await.expect("stream task") {
        Err(Error::Cancelled) => {}
        other => panic!("expected Cancelled, got: {other:?}"),
    }
    // Partial content was captured before the cancellation.
    let last = conv.lock().unwrap().last().expect("assistant message").clone();
    assert_eq!(last.content, "partial");
    let _ = handle.abort();
}
