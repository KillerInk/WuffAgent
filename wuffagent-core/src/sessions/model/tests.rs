//! Unit tests for the `model` module (see `super`).

use super::*;

fn msg(role: &str, content: &str) -> Message {
    Message {
        role: role.to_string(),
        content: content.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn empty_assistant() -> Message {
    Message {
        role: "assistant".to_string(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

/// `add_message` must never store a system message — that is how system
/// prompts leaked into the session history before the redesign.
#[test]
fn add_message_rejects_system_role() {
    let mut s = Session::new("t");
    s.add_message(msg("system", "You are a helpful assistant."));
    assert!(s.messages.is_empty(), "system message must not be stored");
    // Non-system messages still work.
    s.add_message(msg("user", "hi"));
    assert_eq!(s.messages.len(), 1);
}

/// sanitize() pulls stray system messages out of the history into the
/// dedicated prompt field, drops empty assistant placeholders, and removes
/// exact duplicates — repairing legacy corrupted files in place.
#[test]
fn sanitize_extracts_system_drops_placeholders_and_dups() {
    let mut s = Session::new("t");
    s.messages.push(msg("system", "You are a helpful assistant."));
    s.messages.push(msg("user", "hi"));
    s.messages.push(empty_assistant());
    s.messages.push(msg("assistant", "hello"));
    // Exact duplicate of the "assistant: hello" message.
    s.messages.push(msg("assistant", "hello"));

    s.sanitize();

    assert!(
        !s.messages.iter().any(|m| m.role == "system"),
        "no system message may remain in the history"
    );
    assert_eq!(s.system_prompt, "You are a helpful assistant.");
    assert!(
        !s.messages.iter().any(|m| {
            m.role == "assistant" && m.content.is_empty() && m.tool_calls.is_none()
        }),
        "empty assistant placeholders must be dropped"
    );
    // user + single assistant (duplicate collapsed).
    assert_eq!(s.messages.len(), 2);
}

/// sanitize() must not overwrite a prompt that is already set — the stored
/// system_prompt field wins over whatever was found in the history.
#[test]
fn sanitize_keeps_existing_system_prompt() {
    let mut s = Session::new("t");
    s.system_prompt = "stored prompt".to_string();
    s.messages.push(msg("system", "legacy prompt"));
    s.messages.push(msg("user", "hi"));
    s.sanitize();
    assert_eq!(s.system_prompt, "stored prompt");
    assert!(s.messages.iter().all(|m| m.role != "system"));
}

/// A two-turn conversation, appended message-by-message the way the agent
/// loop does, stays in order with no system message and no duplication.
#[test]
fn multi_turn_appends_once_in_order() {
    let mut s = Session::new("t");
    // Turn 1
    s.add_message(msg("user", "turn 1 user"));
    s.add_message(msg("assistant", "turn 1 assistant"));
    // Turn 2
    s.add_message(msg("user", "turn 2 user"));
    s.add_message(msg("assistant", "turn 2 assistant"));
    s.sanitize();

    let roles: Vec<_> = s.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
    let contents: Vec<_> = s.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, ["turn 1 user", "turn 1 assistant", "turn 2 user", "turn 2 assistant"]);
}
