//! Unit tests for the `summarizer` module (see `super`), split by area:
//! - `kinds.rs`: per-kind summarizers (build logs, code, file lists)
//! - `trim.rs`: basic `trim_messages` behavior (protected tail, tool pairs)
//! - `freshness.rs`: stale/superseded read_file eviction, last-resort shrink
//! - `age_pass.rs`: protected current reads in the age-based removal

use super::super::config::TrimConfig;
use super::*;

mod age_pass;
mod freshness;
mod kinds;
mod trim;

fn make_config() -> TrimConfig {
    TrimConfig {
        enabled: true,
        max_tool_result_chars: 200,
        max_chain_entries: 50,
        code_max_lines: 10,
        log_max_lines: 8,
        list_max_items: 10,
        stale_file_invalidation: true,
    }
}

fn user_msg(text: &str) -> Message {
    Message {
        role: "user".into(),
        content: text.into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn assistant_msg(text: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: text.into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn assistant_tool_call(call_id: &str, args: &str) -> Message {
    assistant_call_named(call_id, "echo", args)
}

/// Assistant message requesting the named tool with the given raw args.
fn assistant_call_named(call_id: &str, tool_name: &str, args: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: Some(vec![crate::types::ToolCall {
            id: call_id.into(),
            call_type: "function".into(),
            function: crate::types::ToolFunction {
                name: tool_name.into(),
                arguments: args.into(),
            },
        }]),
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn tool_result(call_id: &str, text: &str) -> Message {
    Message {
        role: "tool".into(),
        content: text.into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: Some(call_id.into()),
        reasoning_content: None,
        image: None,
    }
}

/// Every assistant tool-call id must have a matching tool result present in
/// the trimmed list, and every tool result's id must match a present call.
fn assert_pairs_intact(messages: &[Message]) {
    let call_ids: std::collections::HashSet<&str> = messages
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|tc| tc.id.as_str())
        .collect();
    for m in messages.iter().filter(|m| m.role == "tool") {
        let id = m.tool_call_id.as_deref().unwrap_or("");
        assert!(
            call_ids.contains(id),
            "tool result {} present without its paired tool call",
            id
        );
    }
}

/// Content of the tool result paired with `call_id` (panics if missing).
fn tool_content_at<'a>(messages: &'a [Message], call_id: &str) -> &'a str {
    messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some(call_id))
        .map(|m| m.content.as_str())
        .expect("tool result missing")
}
