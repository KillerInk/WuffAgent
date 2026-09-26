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
    let agent = Agent::builder(config, llm_client, client)
        .tool_manager(tool_manager)
        .build();
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

/// The optional `sub_session` parameter: present-and-true is carried into
/// the request; absent (the default in-turn handoff) stays false.
#[tokio::test]
async fn test_handoff_tool_sub_session_param() {
    use crate::tools::types::ToolParams;

    let (agent, dir) = make_agent_with_handoff(true, vec!["coder".to_string()], "subsess_param_on");
    let params = ToolParams {
        values: serde_json::to_value(serde_json::json!({
            "agent": "coder",
            "task": "Do it",
            "sub_session": true,
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
    drop(tm);
    assert!(result.is_ok(), "handoff tool call should succeed: {:?}", result.err());
    let req = agent
        .take_pending_handoff()
        .expect("pending handoff expected");
    assert!(req.sub_session, "sub_session: true must be carried into the request");

    // Absent → default in-turn handoff.
    let (agent2, dir2) = make_agent_with_handoff(true, vec!["coder".to_string()], "subsess_param_off");
    let params = ToolParams {
        values: serde_json::to_value(serde_json::json!({
            "agent": "coder",
            "task": "Do it",
        }))
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    };
    let tm = agent2.tool_manager.lock().unwrap();
    let result = tm.execute("handoff", params).await;
    drop(tm);
    assert!(result.is_ok(), "handoff tool call should succeed: {:?}", result.err());
    let req2 = agent2
        .take_pending_handoff()
        .expect("pending handoff expected");
    assert!(!req2.sub_session, "an absent sub_session must default to false");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// A `sub_session` handoff ends the parent turn without chaining: the parent
/// store gets the fork marker, the parent gets its terminal `StreamComplete`
/// (clears generating state + persists the parent), and the UI gets
/// `SubSessionHandoff` with both ids — no target agent ever runs (NoopLlm
/// would otherwise complete the first hop immediately).
#[tokio::test]
async fn test_execute_sub_session_handoff_forks_without_chaining() {
    use crate::agents::types::HandoffRequest;
    use crate::types::AppEvent;

    let dir = handoff_agents_dir("subsess_exec");
    let mut config = AgentConfig {
        name: "planner".to_string(),
        ..Default::default()
    };
    config.handoff_enabled = true;
    config.handoff_targets = vec!["coder".to_string()];
    config.agents_dir = dir.clone();

    let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
    let llm = Arc::new(NoopLlm);
    let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    let mut agent = Agent::builder(config, llm, client)
        .tool_manager(tool_manager)
        .event_tx(Some(Arc::new(Mutex::new(tx))))
        .agent_session_id(Some("parent-sid".to_string()))
        .build();

    // Pre-write the request the `handoff` tool would have written during a
    // tool round. The loop picks it up at the top of the first iteration —
    // no LLM call is made at all.
    *agent
        .handoff_mailbox
        .as_ref()
        .expect("handoff mailbox expected")
        .lock()
        .unwrap() = Some(HandoffRequest {
        agent: "coder".to_string(),
        config: AgentConfig {
            name: "coder".to_string(),
            system_prompt: "Code things.".to_string(),
            ..Default::default()
        },
        task: "Implement the plan.".to_string(),
        sub_session: true,
    });

    let result = agent
        .execute("original request", None, &CancellationToken::new())
        .await
        .expect("execute must succeed");
    assert!(
        result.contains("sub-session"),
        "sub-session handoff should report the fork: {result:?}"
    );

    // The parent store holds exactly the user turn + the fork marker — no
    // assistant/tool rounds from a chained target agent.
    let store = agent.client.conversation().lock().unwrap();
    assert_eq!(store.len(), 2, "parent store: {store:?}");
    assert_eq!(store[0].content, "original request");
    assert_eq!(
        store[1].content,
        "[Handed off to 'coder' (sub-session)] Implement the plan."
    );

    // Event order matters: the parent's terminal StreamComplete FIRST (so
    // the parent is idle + saved before the sub-session runs), then the
    // SubSessionHandoff that tells the UI to fork it. `try_iter` (non-
    // blocking): the agent still holds a clone of the sender, so `iter()`
    // would block forever.
    let events: Vec<AppEvent> = rx.try_iter().collect();
    assert_eq!(events.len(), 2, "events: {events:?}");
    match &events[0] {
        AppEvent::StreamComplete {
            content,
            usage,
            session_id,
        } => {
            assert_eq!(session_id, "parent-sid");
            assert!(
                content.starts_with("[Handed off to 'coder' (sub-session)]"),
                "fallback content should be the fork marker: {content:?}"
            );
            assert!(usage.is_none());
        }
        other => panic!("expected StreamComplete first: {other:?}"),
    }
    match &events[1] {
        AppEvent::SubSessionHandoff {
            parent_session_id,
            agent,
            task,
        } => {
            assert_eq!(parent_session_id, "parent-sid");
            assert_eq!(agent, "coder");
            assert_eq!(task, "Implement the plan.");
        }
        other => panic!("expected SubSessionHandoff second: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
