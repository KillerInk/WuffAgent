//! Unit tests for the `handoff` module (see `super`).

use super::*;

/// Temp agents dir with an enabled "coder", an enabled "planner" (which
/// may only hand off to "coder"), and a disabled "ghost".
fn fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent_test_handoff_tool_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("coder.json"),
        r#"{"name":"coder","system_prompt":"Code things."}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("planner.json"),
        r#"{"name":"planner","system_prompt":"Plan things.","handoff_enabled":true,"handoff_targets":["coder"]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("ghost.json"),
        r#"{"name":"ghost","system_prompt":"Ghosts.","enabled":false}"#,
    )
    .unwrap();
    dir
}

fn tool(
    dir: &PathBuf,
    search_dirs: Vec<PathBuf>,
    targets: Vec<String>,
) -> (HandoffTool, Arc<Mutex<Option<HandoffRequest>>>) {
    let mailbox = Arc::new(Mutex::new(None));
    let tool = HandoffTool::new(mailbox.clone(), dir.clone(), search_dirs, targets);
    (tool, mailbox)
}

fn params(agent: &str, task: &str) -> ToolParams {
    ToolParams {
        values: serde_json::to_value(serde_json::json!({
            "agent": agent,
            "task": task,
        }))
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    }
}

#[test]
fn test_handoff_resolves_target_and_writes_mailbox() {
    let dir = fixture_dir("resolve");
    let (t, mailbox) = tool(&dir, vec![], vec![]);

    let result = t.execute(params("coder", "Implement the plan."));
    assert!(result.is_ok(), "unexpected error: {:?}", result);

    let req = mailbox.lock().unwrap().take().expect("request written");
    assert_eq!(req.agent, "coder");
    assert_eq!(req.config.name, "coder");
    assert_eq!(req.task, "Implement the plan.");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_unknown_agent_errors() {
    let dir = fixture_dir("unknown");
    let (t, mailbox) = tool(&dir, vec![], vec![]);

    let err = t.execute(params("nope", "Task")).unwrap_err();
    match err {
        ToolError::InvalidParams(msg) => {
            assert!(msg.contains("nope"), "error should name the agent: {}", msg);
            assert!(msg.contains("coder"), "error should list available agents: {}", msg);
        }
        other => panic!("expected InvalidParams, got {:?}", other),
    }
    assert!(mailbox.lock().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_disabled_target_errors() {
    let dir = fixture_dir("disabled");
    let (t, mailbox) = tool(&dir, vec![], vec![]);

    let err = t.execute(params("ghost", "Task")).unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
    assert!(mailbox.lock().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_target_allowlist_enforced() {
    let dir = fixture_dir("allowlist");
    // planner may only hand off to coder.
    let (t, mailbox) = tool(&dir, vec![], vec!["coder".to_string()]);

    assert!(t.execute(params("coder", "Task")).is_ok());
    assert!(mailbox.lock().unwrap().is_some());

    // Fresh mailbox: hand off to a profile outside the allowlist.
    *mailbox.lock().unwrap() = None;
    let (t2, mailbox2) = tool(&dir, vec![], vec!["coder".to_string()]);
    let err = t2.execute(params("planner", "Task")).unwrap_err();
    match err {
        ToolError::InvalidParams(msg) => {
            assert!(msg.contains("only hand off to"), "{}", msg);
        }
        other => panic!("expected InvalidParams, got {:?}", other),
    }
    assert!(mailbox2.lock().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_rejects_second_pending() {
    let dir = fixture_dir("pending");
    let (t, mailbox) = tool(&dir, vec![], vec![]);

    assert!(t.execute(params("coder", "First")).is_ok());
    let err = t.execute(params("coder", "Second")).unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    assert!(mailbox.lock().unwrap().is_some(), "original request kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_missing_agent_param() {
    let dir = fixture_dir("missing");
    let (t, _) = tool(&dir, vec![], vec![]);

    let p = ToolParams {
        values: serde_json::to_value(serde_json::json!({ "task": "x" }))
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    };
    assert!(matches!(t.execute(p), Err(ToolError::InvalidParams(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Temp dir holding a single enabled profile (distinct prompt so dedup
/// tests can tell the copies apart).
fn search_fixture_dir(tag: &str, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent_test_handoff_search_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{}.json", name)),
        format!(r#"{{"name":"{name}","system_prompt":"{name} things (search dir)."}}"#),
    )
    .unwrap();
    dir
}

#[test]
fn test_handoff_finds_agent_in_search_dir() {
    // "architect" lives ONLY in the search dir (the primary fixture has
    // coder/planner/ghost) — the old single-dir scan found it nowhere.
    let primary = fixture_dir("search-primary");
    let search = search_fixture_dir("architect", "architect");
    let (t, mailbox) = tool(&primary, vec![search.clone()], vec![]);

    assert!(
        t.description().contains("architect"),
        "description must list the search-dir agent: {}",
        t.description()
    );

    t.execute(params("architect", "Implement the plan.")).unwrap();
    let req = mailbox.lock().unwrap().take().expect("request written");
    assert_eq!(req.agent, "architect");
    // Anchored for chained handoffs: the found dir becomes the primary,
    // the rest stay as search dirs.
    assert_eq!(req.config.agents_dir, search);
    assert_eq!(req.config.agents_search_dirs, vec![primary.clone()]);
    let _ = std::fs::remove_dir_all(&primary);
    let _ = std::fs::remove_dir_all(&search);
}

#[test]
fn test_handoff_dedup_primary_wins() {
    let primary = fixture_dir("dedup-primary"); // has "coder" (primary prompt)
    let search = search_fixture_dir("dedup", "coder"); // has "coder" too (search prompt)
    let (t, mailbox) = tool(&primary, vec![search.clone()], vec![]);

    t.execute(params("coder", "Task")).unwrap();
    let req = mailbox.lock().unwrap().take().unwrap();
    assert_eq!(
        req.config.system_prompt, "Code things.",
        "the primary dir's profile must win dedup"
    );
    assert_eq!(req.config.agents_dir, primary);
    let _ = std::fs::remove_dir_all(&primary);
    let _ = std::fs::remove_dir_all(&search);
}

#[test]
fn test_handoff_description_restricted_to_allowlist() {
    let dir = fixture_dir("restrict");
    let (t, _) = tool(&dir, vec![], vec!["coder".to_string()]);

    assert!(t.description().contains("coder"), "{}", t.description());
    assert!(
        !t.description().contains("planner"),
        "disallowed agents must not be advertised: {}",
        t.description()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
