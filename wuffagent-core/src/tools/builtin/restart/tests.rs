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
fn test_is_relaunch_build_detects_target_pair() {
    assert!(is_relaunch_build(
        std::path::Path::new("M:/repos/WuffAgent/target/relaunch/debug/wuffagent-egui.exe")
    ));
    assert!(!is_relaunch_build(
        std::path::Path::new("M:/repos/WuffAgent/target/debug/wuffagent-egui.exe")
    ));
    // A bare `relaunch` component (without `target/` before it) is not the
    // secondary build dir.
    assert!(!is_relaunch_build(std::path::Path::new("C:/other/relaunch/x.exe")));
}

#[test]
fn test_plan_self_restart_requires_wuffagent_exe() {
    // A non-WuffAgent binary (e.g. a test harness) never gets an auto-plan.
    assert!(plan_self_restart(std::path::Path::new("target/debug/some-test-harness.exe")).is_none());
}

#[test]
fn test_plan_self_restart_switches_between_two_builds() {
    // Test cwd is the crate root (which has a Cargo.toml), so the walk-up from
    // a relative path finds a repo root. Only assert on the build command and
    // the target half of the exe path, which are environment-independent.
    let on_relaunch = plan_self_restart(std::path::Path::new("target/relaunch/debug/wuffagent-egui"))
        .expect("plan for a WuffAgent exe running the secondary build");
    assert_eq!(on_relaunch.build_cmd, "cargo build");
    let exe = on_relaunch.exe_path.to_string_lossy().replace('\\', "/");
    assert!(exe.contains("/debug/"), "default build target: {}", exe);
    assert!(!exe.contains("/relaunch/"), "must NOT target the relaunch dir: {}", exe);

    let on_default = plan_self_restart(std::path::Path::new("target/debug/wuffagent-egui"))
        .expect("plan for a WuffAgent exe running the default build");
    assert_eq!(on_default.build_cmd, "cargo build --target-dir target/relaunch");
    let exe = on_default.exe_path.to_string_lossy().replace('\\', "/");
    assert!(exe.contains("/relaunch/debug/"), "secondary build target: {}", exe);
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
