use super::*;

#[test]
fn test_suggestion_serialization() {
    let s = ImprovementSuggestion {
        agent_name: "coder".to_string(),
        prompt_change: Some("You are a coding agent.".to_string()),
        rationale: "Better clarity".to_string(),
        description: Some("A focused coding specialist.".to_string()),
        new_agents: vec![],
        allowed_tools: Some(vec!["file_io".to_string()]),
        reasoning_effort: Some(crate::types::ReasoningEffort::Medium),
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: Some(90_000),
        evidence: vec!["Trajectory: 1 tool calls (0 errors)".to_string()],
    };
    let json = serde_json::to_string(&s).unwrap();
    let parsed: ImprovementSuggestion = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.agent_name, "coder");
    assert_eq!(
        parsed.prompt_change,
        Some("You are a coding agent.".to_string())
    );
    // I2: the wider fields survive a round trip.
    assert_eq!(parsed.allowed_tools, Some(vec!["file_io".to_string()]));
    assert_eq!(
        parsed.reasoning_effort,
        Some(crate::types::ReasoningEffort::Medium)
    );
    assert_eq!(parsed.task_timeout_ms, Some(90_000));
    assert_eq!(parsed.evidence.len(), 1);
    // 2c: the description field round-trips too.
    assert_eq!(
        parsed.description,
        Some("A focused coding specialist.".to_string())
    );
}

#[test]
fn test_suggestion_serialization_without_description() {
    // Old suggestion JSON (no `description` key) still parses (serde default).
    let json = r#"{"agent_name":"coder","rationale":"r","prompt_change":null,"new_agents":[],"allowed_tools":null,"reasoning_effort":null,"shell_config":null,"handoff_targets":null,"task_timeout_ms":null,"evidence":[]}"#;
    let parsed: ImprovementSuggestion = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.description, None);
}

#[test]
fn test_new_agent_proposal_serialization() {
    let prop = NewAgentProposal {
        name: "researcher".to_string(),
        description: "Searches web".to_string(),
        system_prompt: "You are a researcher.".to_string(),
        allowed_tools: vec!["web_search".to_string()],
    };
    let json = serde_json::to_string(&prop).unwrap();
    let parsed: NewAgentProposal = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "researcher");
    assert_eq!(parsed.allowed_tools, vec!["web_search"]);
}
