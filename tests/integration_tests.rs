//! Integration tests for WuffAgent
//!
//! These tests exercise config persistence, client-server interaction via
//! a minimal TCP mock, and error handling paths.

use std::net::SocketAddr;
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
        ChatMessage { role: "user".to_string(), content: "Hi".to_string(), timestamp: "00:00:00".to_string() },
        ChatMessage { role: "assistant".to_string(), content: "Hello!".to_string(), timestamp: "00:00:01".to_string() },
    ];
    cfg.file_path = path.clone();

    cfg.save().unwrap();
    let loaded = Config::load(&path).unwrap();

    assert_eq!(loaded.server_path, "C:\\llama\\llama-server.exe");
    assert_eq!(loaded.port, 18080);
    assert!(!loaded.streaming);
    assert_eq!(loaded.theme, "light");
}

// ---------------------------------------------------------------------------
// Test: Client sends a message and receives a response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_client_send_message() {
    use wuffagent::client::ChatClient;
    
    // Start a mock server
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    
    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read request
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let _request = String::from_utf8_lossy(&buf[..n]);
        
        // Send proper HTTP response
        let response = r#"{"choices":[{"message":{"role":"assistant","content":"Hello from mock!"}}],"usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response.len(),
            response
        );
        let _ = socket.write_all(http_response.as_bytes()).await;
    });
    
    let client = ChatClient::new(&format!("http://{}", mock_addr));
    let result = client.send_message("Hi").await;
    
    assert!(result.is_ok());
    let (content, usage) = result.unwrap();
    assert_eq!(content, "Hello from mock!");
    assert!(usage.is_some());
    
    server_handle.await.unwrap();
}

// ---------------------------------------------------------------------------
// Test: Client handles server error response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_client_server_error() {
    use wuffagent::client::ChatClient;
    
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    
    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let response = r#"{"error":"Something went wrong"}"#;
        let http_response = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response.len(),
            response
        );
        let _ = socket.write_all(http_response.as_bytes()).await;
    });
    
    let client = ChatClient::new(&format!("http://{}", mock_addr));
    let result = client.send_message("Hi").await;
    
    assert!(result.is_err());
    
    server_handle.await.unwrap();
}

// ---------------------------------------------------------------------------
// Test: Client sends tools in request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_client_send_tools() {
    use wuffagent::client::ChatClient;
    
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    
    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);
        
        // Verify tools are in the request
        assert!(request.contains("\"tools\""));
        
        let response = r#"{"choices":[{"message":{"role":"assistant","content":"OK"}}],"usage":null}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response.len(),
            response
        );
        let _ = socket.write_all(http_response.as_bytes()).await;
    });
    
    let client = ChatClient::new(&format!("http://{}", mock_addr));
    
    // Build a minimal tool definition matching the struct fields
    let tools = vec![wuffagent::tools::lib::ToolDefinition {
        type_name: "function".to_string(),
        function: wuffagent::tools::lib::ToolFunctionSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: wuffagent::tools::lib::JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: vec![],
            },
        },
    }];
    let result = client.send_message_with_tools("Hi", Some(&tools)).await;
    
    assert!(result.is_ok());
    server_handle.await.unwrap();
}

// ---------------------------------------------------------------------------
// Test: Client handles empty response choices
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_client_empty_response() {
    use wuffagent::client::ChatClient;
    
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    
    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let response = r#"{"choices":[],"usage":null}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response.len(),
            response
        );
        let _ = socket.write_all(http_response.as_bytes()).await;
    });
    
    let client = ChatClient::new(&format!("http://{}", mock_addr));
    let result = client.send_message("Hi").await;
    
    assert!(result.is_err());
    
    server_handle.await.unwrap();
}

// ---------------------------------------------------------------------------
// Test: Session persistence round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_session_persistence() {
    use wuffagent::sessions::{create_session, save_session, load_session};
    use wuffagent::types::Message;
    
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "Test Session");
    assert_eq!(session.name, "Test Session");
    assert!(!session.id.is_empty());

    let mut loaded = load_session(dir_path, &session.id).unwrap();
    loaded.add_message(Message {
        role: "user".to_string(),
        content: "Hello".to_string(),
        timestamp: String::new(),
        tool_calls: None,
    });
    loaded.add_message(Message {
        role: "assistant".to_string(),
        content: "Hi there!".to_string(),
        timestamp: String::new(),
        tool_calls: None,
    });
    save_session(dir_path, &loaded).unwrap();

    let reloaded = load_session(dir_path, &session.id).unwrap();
    assert_eq!(reloaded.messages.len(), 2);
    assert_eq!(reloaded.messages[0].content, "Hello");
    assert_eq!(reloaded.messages[1].content, "Hi there!");
}
