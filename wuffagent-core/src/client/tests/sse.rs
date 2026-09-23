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
