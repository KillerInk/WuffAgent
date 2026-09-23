use super::*;

#[test]
fn test_disabled_shell_removed_from_agent_schema() {
    let agent = make_agent_with_shell(false);
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        !names.contains(&"shell".to_string()),
        "a disabled shell should not be advertised: {:?}",
        names
    );
}

#[test]
fn test_enabled_shell_present_in_agent_schema() {
    let agent = make_agent_with_shell(true);
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        names.contains(&"shell".to_string()),
        "an enabled shell should be advertised: {:?}",
        names
    );
}


#[test]
fn test_agent_creation() {
    let agent = make_agent("test");
    assert_eq!(agent.config.name, "test");
}

#[test]
fn test_agent_per_agent_reasoning_effort() {
    let llm_client = Arc::new(NoopLlm);
    let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
    // Global client set to Medium.
    let global_client = Arc::new({
        let mut c = ChatClient::new("http://localhost:1");
        c.set_reasoning_effort(crate::types::ReasoningEffort::Medium);
        c
    });

    // Agent with High: gets its own client clone with High.
    let mut config = AgentConfig::default();
    config.name = "researcher".to_string();
    config.reasoning_effort = crate::types::ReasoningEffort::High;
    let agent = Agent::new(
        config,
        llm_client.clone(),
        tool_manager.clone(),
        None,
        global_client.clone(),
        None,
        None,
    );
    assert_eq!(
        agent.client.reasoning_effort(),
        crate::types::ReasoningEffort::High
    );
    assert!(!Arc::ptr_eq(&agent.client, &global_client));

    // Agent with Off: shares the global client (inherits Medium).
    let mut config = AgentConfig::default();
    config.name = "coder".to_string();
    config.reasoning_effort = crate::types::ReasoningEffort::Off;
    let agent = Agent::new(
        config,
        llm_client,
        tool_manager,
        None,
        global_client.clone(),
        None,
        None,
    );
    assert_eq!(
        agent.client.reasoning_effort(),
        crate::types::ReasoningEffort::Medium
    );
    assert!(Arc::ptr_eq(&agent.client, &global_client));
}
