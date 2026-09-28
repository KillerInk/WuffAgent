//! Unit tests for the `hand_back` wiring in `Agent` (injection + execute
//! branch) — the tool-level tests live in tools/builtin/hand_back/tests.rs.

use super::*;
use crate::agents::types::{ControlRequest, HandBackRequest};
use crate::sessions::SessionMeta;
use crate::types::{AppEvent, ReasoningMode};

/// An agent for the given session meta (parent link or not) with a plain
/// config (`hand_back_enabled` defaults to true) and a NoopLlm.
fn agent_with_meta(parent: Option<&str>, hand_back_enabled: bool, sid: &str) -> Agent {
    let client = ChatClient::new("http://localhost:1");
    client.set_session_meta(SessionMeta {
        selected_agent: None,
        reasoning_mode: ReasoningMode::Auto,
        parent_session_id: parent.map(str::to_string),
    });
    let registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let mut config = AgentConfig {
        name: "test".to_string(),
        ..Default::default()
    };
    config.hand_back_enabled = hand_back_enabled;
    Agent::builder(config, Arc::new(NoopLlm), Arc::new(client))
        .tool_manager(Arc::new(ToolManager::new(registry)))
        .agent_session_id(Some(sid.to_string()))
        .build()
}

fn advertised(agent: &Agent) -> Vec<String> {
    agent
        .tool_manager
        .get_tool_definitions()
        .iter()
        .map(|d| d.function.name.clone())
        .collect()
}

/// The tool is advertised exactly when the profile allows it AND the session
/// is a sub-session (meta carries a parent link).
#[test]
fn test_hand_back_injection_gating() {
    // Sub-session + enabled → advertised, empty control mailbox.
    let agent = agent_with_meta(Some("parent-1"), true, "sub-1");
    assert!(
        advertised(&agent).contains(&"hand_back".to_string()),
        "sub-session agent must get hand_back"
    );
    assert!(
        agent.drain_control().is_empty(),
        "nothing should be queued after build"
    );

    // Top-level session (no parent) → absent, nothing queued.
    let agent = agent_with_meta(None, true, "top-1");
    assert!(
        !advertised(&agent).contains(&"hand_back".to_string()),
        "top-level agents must not see hand_back"
    );
    assert!(
        agent.drain_control().is_empty(),
        "a disabled hand_back should queue nothing"
    );

    // Sub-session but the profile opted out → absent, nothing queued.
    let agent = agent_with_meta(Some("parent-2"), false, "sub-2");
    assert!(
        !advertised(&agent).contains(&"hand_back".to_string()),
        "hand_back_enabled=false must suppress the tool"
    );
    assert!(
        agent.drain_control().is_empty(),
        "an opted-out hand_back should queue nothing"
    );
}

/// The mailbox is drained exactly once by `drain_control`.
#[test]
fn test_hand_back_mailbox_consumed_once() {
    let agent = agent_with_meta(Some("parent-3"), true, "sub-3");
    agent
        .control_mailbox
        .lock()
        .unwrap()
        .push(ControlRequest::HandBack(HandBackRequest {
            task: "Resume the plan.".to_string(),
        }));

    let req = agent
        .drain_control()
        .into_iter()
        .find_map(|r| r.as_hand_back().cloned())
        .expect("pending hand-back expected");
    assert_eq!(req.task, "Resume the plan.");
    assert!(
        agent.drain_control().is_empty(),
        "mailbox is consumed exactly once"
    );
}

/// A `hand_back` request ends the sub-session turn: marker in the SUB store,
/// terminal `StreamComplete` for the sub sid, then `AgentHandBack` carrying
/// both session ids (sub first, so the sub is idle + saved before the parent
/// receives the task). No LLM call is made (the request is pre-written the
/// way the `hand_back` tool would have done it in a tool round).
#[tokio::test]
async fn test_execute_hand_back_marker_and_event() {
    let client = ChatClient::new("http://localhost:1");
    client.set_session_meta(SessionMeta {
        selected_agent: None,
        reasoning_mode: ReasoningMode::Auto,
        parent_session_id: Some("parent-sid".to_string()),
    });
    let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
    let registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let mut agent = Agent::builder(
        AgentConfig {
            name: "coder".to_string(),
            ..Default::default()
        },
        Arc::new(NoopLlm),
        Arc::new(client),
    )
    .tool_manager(Arc::new(ToolManager::new(registry)))
    .event_tx(Some(Arc::new(Mutex::new(tx))))
    .agent_session_id(Some("sub-sid".to_string()))
    .build();

    agent
        .control_mailbox
        .lock()
        .unwrap()
        .push(ControlRequest::HandBack(HandBackRequest {
            task: "Implement the plan.".to_string(),
        }));

    let result = agent
        .execute("original request", None, &CancellationToken::new())
        .await
        .expect("execute must succeed");
    assert!(
        result.contains("parent session"),
        "hand-back should report the return: {result:?}"
    );

    // The sub store holds exactly the user turn + the return marker.
    let store = agent.client.conversation().lock().unwrap();
    assert_eq!(store.len(), 2, "sub store: {store:?}");
    assert_eq!(store[0].content, "original request");
    assert_eq!(
        store[1].content,
        "[Handed back to parent session] Implement the plan."
    );

    // Event order: StreamComplete for the sub first, then AgentHandBack.
    let events: Vec<AppEvent> = rx.try_iter().collect();
    assert_eq!(events.len(), 2, "events: {events:?}");
    match &events[0] {
        AppEvent::StreamComplete {
            content,
            usage,
            session_id,
        } => {
            assert_eq!(session_id, "sub-sid");
            assert!(
                content.starts_with("[Handed back to parent session]"),
                "fallback content should be the return marker: {content:?}"
            );
            assert!(usage.is_none());
        }
        other => panic!("expected StreamComplete first: {other:?}"),
    }
    match &events[1] {
        AppEvent::AgentHandBack {
            from_session_id,
            to_session_id,
            task,
        } => {
            assert_eq!(from_session_id, "sub-sid");
            assert_eq!(to_session_id, "parent-sid");
            assert_eq!(task, "Implement the plan.");
        }
        other => panic!("expected AgentHandBack second: {other:?}"),
    }
}
