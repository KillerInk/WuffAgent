//! Integration tests for WuffAgent
//!
//! These tests exercise config persistence, client-server interaction via
//! a minimal TCP mock, and error handling paths.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Test: Config persistence via temp file round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_config_persistence() {
    use wuffagent::config::{Config, ChatMessage};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");

    let mut cfg = Config::default();
    cfg.server_path = "C:\\llama\\llama-server.exe".to_string();
    cfg.model_path = "C:\\models\\model.gguf".to_string();
    cfg.port = 18080;
    cfg.threads = 4;
    cfg.streaming = false;
    cfg.theme = "light".to_string();
    cfg.chat_history = vec![
        ChatMessage { role: "user".to_string(), content: "Hi".to_string() },
        ChatMessage { role: "assistant".to_string(), content: "Hello!".to_string() },
    ];
    cfg.file_path = path.clone();

    cfg.save().unwrap();
    let loaded = Config::load(&path).unwrap();

    assert_eq!(loaded.server_path, "C:\\llama\\llama-server.exe");
    assert_eq!(loaded.port, 18080);
    assert!(!loaded.streaming);
    assert_eq!(loaded.theme, "light");
    assert_eq!(loaded.chat_history.len(), 2);
    assert_eq!(loaded.chat_history[0].content, "Hi");
    assert_eq!(loaded.chat_history[1].content, "Hello!");
}

// ---------------------------------------------------------------------------
// Test: ChatClient sends a non-streaming request to a mock server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_chat_client_request() {
    use wuffagent::client::ChatClient;

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let _request_str = String::from_utf8_lossy(&buf[..n]);

        let response_body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Mock response"
                }
            }]
        })
        .to_string();

        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            response_body.len(),
            response_body
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });

    let base_url = format!("http://127.0.0.1:{}", port);
    let client = ChatClient::new(&base_url);
    let http_client = client.http_client.clone();
    let conversation = client.conversation.clone();

    let result = ChatClient::send_message(
        &base_url,
        "",
        conversation.clone(),
        &http_client,
        None,
        "Hello",
    )
    .await;

    assert!(result.is_ok());
    let content = result.unwrap();
    assert_eq!(content, "Mock response");

    // Verify conversation history was updated
    let conv = conversation.lock().unwrap();
    assert_eq!(conv.len(), 2);
    assert_eq!(conv[0].role, "user");
    assert_eq!(conv[0].content, "Hello");
    assert_eq!(conv[1].role, "assistant");
    assert_eq!(conv[1].content, "Mock response");

    let _ = server_handle.await;
}

// ---------------------------------------------------------------------------
// Test: Error handling - connect to non-existent server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_chat_client_error_handling() {
    use wuffagent::client::ChatClient;

    let client = ChatClient::new("http://127.0.0.1:1");
    let http_client = client.http_client.clone();
    let conversation = client.conversation.clone();

    let result = ChatClient::send_message(
        "http://127.0.0.1:1",
        "",
        conversation,
        &http_client,
        None,
        "Hello",
    )
    .await;

    // Should fail because no server is running on port 1
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Test: ServerManager construction and initial state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_manager_state() {
    use wuffagent::server::ServerManager;

    let server = ServerManager::new(
        "llama-server",
        "test_model.gguf",
        18080,
        0,
        2048,
        4,
    );

    assert!(!server.is_running());
    assert!(server.get_error().is_none());
}

// ---------------------------------------------------------------------------
// Test: SSE parsing end-to-end with mock server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sse_parsing_end_to_end() {
    use wuffagent::client::ChatClient;

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();

        let sse_data = vec![
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "Cache-Control: no-cache\r\n",
            "Connection: keep-alive\r\n",
            "\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n",
            "data: [DONE]\n",
        ].concat();

        let _ = socket.write_all(sse_data.as_bytes()).await;
        let _ = socket.flush().await;
    });

    let base_url = format!("http://127.0.0.1:{}", port);
    let client = ChatClient::new(&base_url);
    let http_client = client.http_client.clone();
    let conversation = client.conversation.clone();

    let received_chunks = Arc::new(Mutex::new(Vec::new()));
    let chunks_clone = received_chunks.clone();

    let result = ChatClient::stream_message(
        &base_url,
        "",
        conversation.clone(),
        &http_client,
        None,
        "Hello",
        move |chunk| {
            chunks_clone.lock().unwrap().push(chunk);
            Ok(())
        },
    )
    .await;

    assert!(result.is_ok());
    let chunks = received_chunks.lock().unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], "Hello");
    assert_eq!(chunks[1], " world");

    // Verify conversation history
    let conv = conversation.lock().unwrap();
    assert_eq!(conv.len(), 2);
    assert_eq!(conv[0].content, "Hello");
    assert_eq!(conv[1].content, "Hello world");

    let _ = server_handle.await;
}
