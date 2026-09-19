//! Unit tests for the `improvements` module (see `super`).

use super::*;

fn temp_agents_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent-egui-imp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn existing_agent(name: &str) -> AgentConfig {
    AgentConfig {
        name: name.to_string(),
        description: "existing".to_string(),
        system_prompt: "old prompt".to_string(),
        ..Default::default()
    }
}

#[test]
fn test_apply_improvement_updates_prompt_and_creates_agent() {
    let dir = temp_agents_dir("update");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let imp = PendingImprovement {
        agent_name: "coder".to_string(),
        prompt_change: Some("new prompt".to_string()),
        rationale: "test".to_string(),
        new_agents: vec![wuffagent_core::memory::NewAgentProposal {
            name: "helper".to_string(),
            description: "a helper".to_string(),
            system_prompt: "helper prompt".to_string(),
            allowed_tools: vec!["file_io".to_string()],
        }],
    };

    let msg = apply_improvement(&manager, &imp);
    assert!(msg.contains("updated prompt for 'coder'"), "msg: {}", msg);
    assert!(msg.contains("created new agent 'helper'"), "msg: {}", msg);

    // Prompt change persisted.
    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.system_prompt, "new prompt");
    // New agent file written and parseable.
    let helper = manager.get_agent("helper").expect("helper agent");
    assert_eq!(helper.system_prompt, "helper prompt");
    assert_eq!(helper.allowed_tools, vec!["file_io"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_improvement_missing_agent_reports_error() {
    let dir = temp_agents_dir("missing");
    let manager = AgentManager::new(dir.clone());

    let imp = PendingImprovement {
        agent_name: "ghost".to_string(),
        prompt_change: Some("p".to_string()),
        rationale: "test".to_string(),
        new_agents: vec![],
    };

    let msg = apply_improvement(&manager, &imp);
    assert!(msg.contains("agent 'ghost' not found"), "msg: {}", msg);

    let _ = std::fs::remove_dir_all(&dir);
}
