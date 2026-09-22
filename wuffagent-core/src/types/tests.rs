//! Unit tests for the `types` module (see `super`).

use super::*;

fn msg(content: &str) -> Message {
    Message {
        role: "user".to_string(),
        content: content.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn msg_with_image(content: &str, url: &str) -> Message {
    Message {
        role: "user".to_string(),
        content: content.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: Some(url.to_string()),
    }
}

/// Without an image, `content` must stay a plain JSON string — the format
/// every existing server and session file expects.
#[test]
fn message_without_image_serializes_plain_content() {
    let json = serde_json::to_string(&msg("hello")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["content"], "hello");
    assert!(v.get("image").is_none(), "no image field on the wire");
}

/// With an image, `content` becomes the OpenAI multimodal parts array and
/// there is no separate `image` field on the wire.
#[test]
fn message_with_image_serializes_content_parts() {
    let url = "data:image/png;base64,AAAA";
    let json = serde_json::to_string(&msg_with_image("describe this", url)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let parts = v["content"].as_array().expect("content must be an array");
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "describe this");
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(parts[1]["image_url"]["url"], url);
    assert!(v.get("image").is_none(), "no image field on the wire");
}

/// An image with empty text serializes as a single image_url part.
#[test]
fn image_only_message_omits_empty_text_part() {
    let url = "data:image/png;base64,AAAA";
    let v: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&msg_with_image("", url)).unwrap()).unwrap();
    let parts = v["content"].as_array().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["type"], "image_url");
}

/// Round trip: image survives serialization + deserialization.
#[test]
fn message_with_image_round_trips() {
    let url = "data:image/jpeg;base64,QUJD";
    let original = msg_with_image("what is this?", url);
    let json = serde_json::to_string(&original).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(back.role, "user");
    assert_eq!(back.content, "what is this?");
    assert_eq!(back.image.as_deref(), Some(url));
    // And the re-serialized form is stable.
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
}

/// Legacy session files stored `content` as a plain string (no image):
/// they must load with `image: None`.
#[test]
fn legacy_string_content_deserializes_with_no_image() {
    let json = r#"{"role":"user","content":"hi","timestamp":"12:00:00"}"#;
    let m: Message = serde_json::from_str(json).unwrap();
    assert_eq!(m.content, "hi");
    assert_eq!(m.timestamp, "12:00:00");
    assert!(m.image.is_none());
}

/// A parts-array content deserializes back into text + image; multiple
/// text parts are joined.
#[test]
fn parts_array_content_deserializes_into_text_and_image() {
    let json = r#"{
        "role": "user",
        "content": [
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,QQ"}}
        ],
        "timestamp": ""
    }"#;
    let m: Message = serde_json::from_str(json).unwrap();
    assert_eq!(m.content, "first\nsecond");
    assert_eq!(m.image.as_deref(), Some("data:image/png;base64,QQ"));
}

/// Timestamp helpers: new format carries a day; legacy time-only does not.
#[test]
fn timestamp_helpers_parse_new_and_legacy() {
    let new = format_timestamp();
    assert_eq!(new.len(), 19, "new format is 'YYYY-MM-DD HH:MM:SS'");
    assert_eq!(timestamp_day(&new).map(str::len), Some(10));
    assert_eq!(timestamp_time(&new).len(), 8);

    let legacy = "12:00:00";
    assert_eq!(timestamp_day(legacy), None);
    assert_eq!(timestamp_time(legacy), legacy);

    assert_eq!(timestamp_day(""), None);
    assert_eq!(timestamp_time(""), "");
}

/// Args preview picks the descriptive field per tool.
#[test]
fn tool_args_summary_picks_descriptive_fields() {
    assert_eq!(
        tool_args_summary("shell", r#"{"command":"git log -1 --stat"}"#),
        "git log -1 --stat"
    );
    assert_eq!(
        tool_args_summary("read_file", r#"{"path":"/tmp/a.rs"}"#),
        "/tmp/a.rs"
    );
    assert_eq!(
        tool_args_summary("search_content", r#"{"pattern":"foo","path":"src"}"#),
        "foo in src"
    );
    assert_eq!(
        tool_args_summary("web_search", r#"{"query":"rust async"}"#),
        "rust async"
    );
    // Unknown tool: first preferred field present wins.
    assert_eq!(
        tool_args_summary("mcp_thing", r#"{"url":"https://x.dev"}"#),
        "https://x.dev"
    );
    // Unparseable JSON: flattened raw text.
    assert_eq!(tool_args_summary("x", "hello world"), "hello world");
    // Empty shapes: empty preview.
    assert_eq!(tool_args_summary("shell", ""), "");
    assert_eq!(tool_args_summary("shell", "{}"), "");
    // Long values are truncated with an ellipsis.
    let long = "a".repeat(300);
    let out = tool_args_summary("shell", &format!(r#"{{"command":"{}"}}"#, long));
    assert!(out.chars().count() <= 121);
    assert!(out.ends_with('…'));
}

/// Tool-call fields still round-trip alongside the new layout.
#[test]
fn tool_call_fields_round_trip() {
    let mut m = msg("do it");
    m.role = "assistant".to_string();
    m.tool_calls = Some(vec![ToolCall {
        id: "c1".to_string(),
        call_type: "function".to_string(),
        function: ToolFunction {
            name: "shell".to_string(),
            arguments: "{\"command\":\"ls\"}".to_string(),
        },
    }]);
    let back: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back.tool_calls.as_ref().unwrap()[0].function.name, "shell");
}
