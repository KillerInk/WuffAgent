use super::*;

fn handoff_agents_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent_test_agent_handoff_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("coder.json"),
        r#"{"name":"coder","system_prompt":"Code things."}"#,
    )
    .unwrap();
    dir
}

/// `tag` must be unique per test: the fixture dir is shared with any
/// concurrently running test that uses the same tag, and each test deletes
/// it on cleanup (parallel test runs would race otherwise).
fn make_agent_with_handoff(
    enabled: bool,
    targets: Vec<String>,
    tag: &str,
) -> (Agent, std::path::PathBuf) {
    let dir = handoff_agents_dir(tag);
    let mut config = AgentConfig {
        name: "planner".to_string(),
        ..Default::default()
    };
    config.handoff_enabled = enabled;
    config.handoff_targets = targets;
    config.agents_dir = dir.clone();
    let llm_client = Arc::new(NoopLlm);
    let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    let agent = Agent::new(config, llm_client, tool_manager, None, client, None, None);
    (agent, dir)
}

#[test]
fn test_handoff_tool_injected_when_enabled() {
    let (agent, dir) = make_agent_with_handoff(true, vec!["coder".to_string()], "inject_on");
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        names.contains(&"handoff".to_string()),
        "an enabled handoff should be advertised: {:?}",
        names
    );
    assert!(
        agent.handoff_mailbox.is_some(),
        "an enabled handoff should create a mailbox"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_tool_absent_when_disabled() {
    let (agent, dir) = make_agent_with_handoff(false, vec!["coder".to_string()], "inject_off");
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        !names.contains(&"handoff".to_string()),
        "a disabled handoff should not be advertised: {:?}",
        names
    );
    assert!(
        agent.handoff_mailbox.is_none(),
        "a disabled handoff should not create a mailbox"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_take_pending_handoff() {
    use crate::tools::types::ToolParams;

    let (agent, dir) = make_agent_with_handoff(true, vec!["coder".to_string()], "take_on");
    // Invoke the injected per-execution handoff tool through the manager.
    let params = ToolParams {
        values: serde_json::to_value(serde_json::json!({
            "agent": "coder",
            "task": "Implement the plan.",
        }))
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    };
    let tm = agent.tool_manager.lock().unwrap();
    let result = tm.execute("handoff", params).await;
    assert!(
        result.is_ok(),
        "handoff tool call should succeed: {:?}",
        result.err()
    );
    drop(tm);

    // First take: the request; second take: empty.
    let req = agent
        .take_pending_handoff()
        .expect("pending handoff expected");
    assert_eq!(req.agent, "coder");
    assert_eq!(req.config.name, "coder");
    assert_eq!(req.task, "Implement the plan.");
    assert!(
        agent.take_pending_handoff().is_none(),
        "mailbox is consumed exactly once"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // An agent without handoff never has a mailbox.
    let (agent2, dir2) = make_agent_with_handoff(false, Vec::new(), "take_off");
    assert!(agent2.handoff_mailbox.is_none());
    assert!(agent2.take_pending_handoff().is_none());
    let _ = std::fs::remove_dir_all(&dir2);
}
