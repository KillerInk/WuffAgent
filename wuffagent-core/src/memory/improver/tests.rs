//! Unit tests for the `improver` module (see `super`).

use super::*;
use crate::memory::{MemoryConfig, MemoryEntry, MemoryType};
use tempfile::tempdir;

/// Manager with an isolated temp memories dir (default would hit
/// ~/.wuffagent/memories and leak real entries into the tests).
fn fresh_manager() -> (MemoryManager, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    (MemoryManager::new(config).unwrap(), dir)
}

#[test]
fn test_suggestion_serialization() {
    let s = ImprovementSuggestion {
        agent_name: "coder".to_string(),
        prompt_change: Some("You are a coding agent.".to_string()),
        rationale: "Better clarity".to_string(),
        new_agents: vec![],
    };
    let json = serde_json::to_string(&s).unwrap();
    let parsed: ImprovementSuggestion = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.agent_name, "coder");
    assert_eq!(parsed.prompt_change, Some("You are a coding agent.".to_string()));
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

#[test]
fn test_collect_lessons_prefers_task_relevant_search() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Use cargo check before running tests in the rust workspace",
            "agent",
            &[],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "An unrelated fact about rust crates and packaging",
            "agent",
            &[],
        ))
        .unwrap();

    let lessons = collect_lessons(&manager, "coder", "run the tests in the rust workspace");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("cargo check"));
}

#[test]
fn test_collect_lessons_falls_back_to_recent_when_no_match() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "The shell tool fails when the working directory does not exist",
            "agent",
            &["shell"],
        ))
        .unwrap();

    // No keyword overlap between the query and the stored lesson, so the
    // recent-lessons fallback must surface it instead of skipping the check.
    let lessons = collect_lessons(&manager, "coder", "completely unrelated topic zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("shell tool"));
}

#[test]
fn test_collect_lessons_empty_store() {
    let (manager, _dir) = fresh_manager();
    assert!(collect_lessons(&manager, "coder", "some task").is_empty());
}
