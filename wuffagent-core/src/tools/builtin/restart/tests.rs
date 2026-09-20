//! Unit tests for the `restart` module (see `super`).

use super::*;

fn tool() -> (RestartTool, Arc<Mutex<Option<RestartRequest>>>) {
    let mailbox = Arc::new(Mutex::new(None));
    let t = RestartTool::new(mailbox.clone());
    (t, mailbox)
}

fn params(json: serde_json::Value) -> ToolParams {
    ToolParams {
        values: json
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

#[test]
fn test_restart_writes_mailbox() {
    let (t, mailbox) = tool();
    let result = t.execute(params(serde_json::json!({
        "reason": "added the restart feature"
    })));
    assert!(result.is_ok(), "unexpected error: {:?}", result);

    let req = mailbox.lock().unwrap().take().expect("request written");
    assert_eq!(req.reason, "added the restart feature");
    assert!(req.build_cmd.is_none());
    assert!(req.exe_path.is_none());
}

#[test]
fn test_restart_carries_build_and_exe() {
    let (t, mailbox) = tool();
    t.execute(params(serde_json::json!({
        "reason": "rebuilt",
        "build_cmd": "exit 0",
        "exe_path": "target/relaunch/debug/wuffagent-egui.exe"
    })))
    .unwrap();

    let req = mailbox.lock().unwrap().take().expect("request written");
    assert_eq!(req.build_cmd.as_deref(), Some("exit 0"));
    assert_eq!(req.exe_path.as_deref(), Some("target/relaunch/debug/wuffagent-egui.exe"));
}

#[test]
fn test_restart_missing_reason_errors() {
    let (t, mailbox) = tool();
    let err = t.execute(params(serde_json::json!({ "build_cmd": "exit 0" }))).unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
    assert!(mailbox.lock().unwrap().is_none());
}

#[test]
fn test_restart_empty_reason_errors() {
    let (t, mailbox) = tool();
    let err = t.execute(params(serde_json::json!({ "reason": "   " }))).unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
    assert!(mailbox.lock().unwrap().is_none());
}

#[test]
fn test_restart_rejects_second_pending() {
    let (t, mailbox) = tool();
    assert!(t.execute(params(serde_json::json!({ "reason": "First" }))).is_ok());
    let err = t.execute(params(serde_json::json!({ "reason": "Second" }))).unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    // Original request kept.
    let req = mailbox.lock().unwrap().take().expect("original kept");
    assert_eq!(req.reason, "First");
}

#[test]
fn test_restart_build_failure_skips_mailbox() {
    let (t, mailbox) = tool();
    // A command that exits non-zero (portable across sh and powershell).
    let out = t
        .execute(params(serde_json::json!({
            "reason": "build then restart",
            "build_cmd": "exit 1"
        })))
        .expect("execute returns Ok(ToolOutput) even for a build failure");
    assert!(
        matches!(out, ToolOutput::Error(_)),
        "a failed build must surface as an error output, got {:?}",
        out
    );
    assert!(
        mailbox.lock().unwrap().is_none(),
        "a failed build must NOT queue a restart"
    );
}

#[test]
fn test_restart_build_success_queues_restart() {
    let (t, mailbox) = tool();
    let out = t
        .execute(params(serde_json::json!({
            "reason": "build then restart",
            "build_cmd": "exit 0"
        })))
        .expect("execute returns Ok");
    assert!(matches!(out, ToolOutput::Success(_)), "got {:?}", out);
    let req = mailbox.lock().unwrap().take().expect("request written after successful build");
    assert_eq!(req.build_cmd.as_deref(), Some("exit 0"));
}

#[test]
fn test_restart_schema_shape() {
    let (t, _) = tool();
    let schema = t.parameters_schema();
    assert_eq!(schema.name, "restart");
    let input = schema.input_type.expect("schema has an input object");
    assert_eq!(input.required, vec!["reason".to_string()]);
    let props = input.properties.expect("schema lists properties");
    assert!(props.contains_key("reason"));
    assert!(props.contains_key("build_cmd"));
    assert!(props.contains_key("exe_path"));
}
