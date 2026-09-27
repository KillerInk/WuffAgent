//! Chat-profile support in the improver: "chat" is a synthetic profile (no
//! backing .json file) that can still be the target of a prompt suggestion,
//! and suggestions must not create a profile that already exists.

use super::*;

fn chat_agent_config() -> crate::agents::config::AgentConfig {
    let mut cfg = test_agent_config();
    cfg.name = "chat".to_string();
    cfg.description = "General purpose agent".to_string();
    cfg.system_prompt = "You are the chat agent.".to_string();
    cfg
}

/// The chat profile is an EXISTING profile: the prompt tells the LLM to
/// propose a prompt_change for "chat", never a new_agents entry.
#[tokio::test]
async fn test_chat_profile_instructs_prompt_change_not_new_agent() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Chat responses should cite tool output",
            "agent",
            &["agent:chat"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let stats = crate::agents::RunStats::default();
    let _ = suggest_improvements(
        &manager,
        &chat_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("The profile 'chat' EXISTS as an agent profile (the chat agent)."),
        "prompt must declare 'chat' an existing profile: {}",
        prompt
    );
    assert!(
        prompt.contains("Propose prompt_change for agent_name \"chat\""),
        "prompt must steer toward prompt_change: {}",
        prompt
    );
    assert!(
        !prompt.contains("the chat profile has no backing file"),
        "the synthetic-profile warning must be suppressed for 'chat': {}",
        prompt
    );
}

/// A suggestion for "chat" carries no new-agent proposal.
#[tokio::test]
async fn test_chat_suggestion_is_prompt_change_only() {
    let dir = tempdir().unwrap();
    let (manager, _prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Chat responses should cite tool output",
            "agent",
            &["agent:chat"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: r#"
        [{
            "agent_name": "chat",
            "prompt_change": "You are the chat agent. Cite tool output.",
            "rationale": "cite output"
        }]
        "#
        .to_string(),
        prompts: Arc::new(Mutex::new(Vec::new())),
    });
    let stats = crate::agents::RunStats::default();
    let suggestions = suggest_improvements(
        &manager,
        &chat_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].agent_name, "chat");
    assert!(suggestions[0].prompt_change.is_some());
    assert!(
        suggestions[0].new_agents.is_empty(),
        "chat suggestion must not carry new_agents: {:?}",
        suggestions[0].new_agents
    );
}

/// The re-target guard must NOT re-target a "chat" suggestion onto the
/// reviewed profile — "chat" is an existing (synthetic) profile, so the
/// suggestion keeps its "chat" target even when the review ran for a
/// different profile.
#[tokio::test]
async fn test_retarget_guard_keeps_chat_target() {
    let dir = tempdir().unwrap();
    let (manager, _prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson that triggers the check",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: r#"[{"agent_name": "chat", "prompt_change": "new chat prompt", "rationale": "improve chat"}]"#
            .to_string(),
        prompts: Arc::new(Mutex::new(Vec::new())),
    });
    let stats = crate::agents::RunStats::default();
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(
        suggestions[0].agent_name, "chat",
        "a 'chat' suggestion must keep its target (not be re-targeted to 'coder')"
    );
}

/// The prompt's new-agent section names every EXISTING profile so the LLM
/// never proposes one that already exists (and the roster's descriptions are
/// shown, which a chat-profile review needs to know the fleet it delegates to).
#[tokio::test]
async fn test_fleet_prompt_lists_existing_agent_names() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _metrics = MetricsDirGuard::new();

    let (manager, prompts, _keep) = auto_improve_manager(tempdir().unwrap().path());
    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let roster = vec![
        ("coder".to_string(), "writes code".to_string()),
        ("reviewer".to_string(), "reviews code".to_string()),
    ];
    let _ = suggest_fleet_improvements(&manager, &roster, None, llm.as_ref()).await.unwrap();

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("Existing agent profiles (do NOT propose these as new agents): coder (writes code), reviewer (reviews code)."),
        "existing-agent list missing: {}",
        prompt
    );
}

/// The synthetic chat config carries the profile identity the improver and
/// the run-check paths need (name "chat", non-empty base prompt) so a
/// `run_self_improvement` check for "chat" runs even though no profile file
/// exists; callers with the live app config overwrite the system prompt.
#[test]
fn test_synthetic_chat_config_identity() {
    let cfg = synthetic_chat_config();
    assert_eq!(cfg.name, CHAT_PROFILE_NAME);
    assert!(!cfg.system_prompt.is_empty(), "base prompt must be non-empty");
    assert!(
        cfg.description.contains("chat settings"),
        "description must point at the chat settings as the profile's home: {}",
        cfg.description
    );
}
