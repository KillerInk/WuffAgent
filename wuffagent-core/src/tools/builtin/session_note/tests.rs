use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agents::types::{ControlRequest, SessionNoteRequest};
use crate::tools::builtin::session_note::SessionNoteTool;
use crate::tools::types::{Tool, ToolOutput, ToolParams};

fn tool() -> (SessionNoteTool, Arc<Mutex<Vec<ControlRequest>>>) {
    let mailbox = Arc::new(Mutex::new(Vec::new()));
    (
        SessionNoteTool::new(mailbox.clone()),
        mailbox,
    )
}

/// Pull the queued note (if any) out of the shared control mailbox.
fn take_session_note(
    mailbox: &Arc<Mutex<Vec<ControlRequest>>>,
) -> Option<SessionNoteRequest> {
    let mut guard = mailbox.lock().unwrap();
    guard
        .iter()
        .position(|r| matches!(r, ControlRequest::SessionNote(_)))
        .map(|i| match guard.remove(i) {
            ControlRequest::SessionNote(r) => r,
            _ => unreachable!(),
        })
}

fn params(value: serde_json::Value) -> ToolParams {
    let values: HashMap<String, serde_json::Value> =
        serde_json::from_value(value).expect("object params");
    ToolParams { values }
}

fn is_queued(out: &ToolOutput) -> bool {
    matches!(
        out,
        ToolOutput::Success(v) if v.get("status").and_then(|s| s.as_str()) == Some("session_note_queued")
    )
}

#[test]
fn name_is_session_note() {
    let (t, _) = tool();
    assert_eq!(t.name(), "session_note");
}

#[test]
fn queues_note_in_mailbox() {
    let (t, mailbox) = tool();
    let out = t
        .execute(params(serde_json::json!({"note": "decision: use AppEvent"})))
        .unwrap();
    assert!(is_queued(&out));
    let req = take_session_note(&mailbox).expect("note queued");
    assert_eq!(req.note, "decision: use AppEvent");
    assert!(mailbox.lock().unwrap().is_empty(), "mailbox drained by take()");
}

#[test]
fn trims_whitespace_and_rejects_empty() {
    let (t, mailbox) = tool();
    assert!(
        t.execute(params(serde_json::json!({"note": "  "})))
            .is_err()
    );
    assert!(mailbox.lock().unwrap().is_empty());
    t.execute(params(serde_json::json!({"note": "  pinned  "})))
        .unwrap();
    let guard = mailbox.lock().unwrap();
    let req = guard
        .iter()
        .find_map(|r| match r {
            ControlRequest::SessionNote(r) => Some(r),
            _ => None,
        })
        .expect("note queued");
    assert_eq!(req.note, "pinned");
}

#[test]
fn missing_note_is_invalid_params() {
    let (t, mailbox) = tool();
    assert!(t.execute(params(serde_json::json!({}))).is_err());
    assert!(mailbox.lock().unwrap().is_empty());
}

#[test]
fn second_note_while_one_pending_is_an_error() {
    let (t, mailbox) = tool();
    t.execute(params(serde_json::json!({"note": "first"})))
        .unwrap();
    let err = t
        .execute(params(serde_json::json!({"note": "second"})))
        .unwrap_err();
    assert!(err.to_string().contains("already pending"));
    // The first note is still the pending one.
    let guard = mailbox.lock().unwrap();
    let req = guard
        .iter()
        .find_map(|r| match r {
            ControlRequest::SessionNote(r) => Some(r),
            _ => None,
        })
        .expect("first note still pending");
    assert_eq!(req.note, "first");
}

#[test]
fn schema_requires_note_string() {
    let (t, _) = tool();
    let schema = t.parameters_schema();
    assert_eq!(schema.name, "session_note");
    let input = schema.input_type.as_ref().unwrap();
    let props = input.properties.as_ref().unwrap();
    assert!(props.contains_key("note"));
    assert_eq!(props["note"].type_name, "string");
    assert_eq!(input.required, vec!["note".to_string()]);
}
