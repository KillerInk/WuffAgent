//! Unit tests for the `hand_back` module (see `super`).

use super::*;

fn mailbox() -> Arc<Mutex<Vec<ControlRequest>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// Pull the queued hand-back (if any) out of the shared control mailbox.
fn take_hand_back(mailbox: &Arc<Mutex<Vec<ControlRequest>>>) -> Option<HandBackRequest> {
    let mut guard = mailbox.lock().unwrap();
    guard
        .iter()
        .position(|r| matches!(r, ControlRequest::HandBack(_)))
        .map(|i| match guard.remove(i) {
            ControlRequest::HandBack(r) => r,
            _ => unreachable!(),
        })
}

fn params(task: &str) -> ToolParams {
    ToolParams {
        values: serde_json::to_value(serde_json::json!({ "task": task }))
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

#[test]
fn test_hand_back_writes_mailbox() {
    let mailbox = mailbox();
    let tool = HandBackTool::new(mailbox.clone());

    let result = tool.execute(params("Implement the plan."));
    assert!(result.is_ok(), "unexpected error: {:?}", result);

    let req = take_hand_back(&mailbox).expect("request written");
    assert_eq!(req.task, "Implement the plan.");
}

#[test]
fn test_hand_back_missing_task_errors() {
    let mailbox = mailbox();
    let tool = HandBackTool::new(mailbox.clone());

    let p = ToolParams {
        values: std::collections::HashMap::new(),
    };
    let err = tool.execute(p).unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
    assert!(mailbox.lock().unwrap().is_empty());
}

#[test]
fn test_hand_back_empty_task_errors() {
    let mailbox = mailbox();
    let tool = HandBackTool::new(mailbox.clone());

    let err = tool.execute(params("   ")).unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
    assert!(mailbox.lock().unwrap().is_empty());
}

#[test]
fn test_hand_back_rejects_second_pending() {
    let mailbox = mailbox();
    let tool = HandBackTool::new(mailbox.clone());

    assert!(tool.execute(params("First")).is_ok());
    let err = tool.execute(params("Second")).unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    let req = take_hand_back(&mailbox).expect("original request kept");
    assert_eq!(req.task, "First");
}
