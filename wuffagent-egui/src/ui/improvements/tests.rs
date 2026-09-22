//! Unit tests for the `improvements` module (see `super`).

use super::*;

fn temp_agents_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-egui-imp-{tag}-{}",
        std::process::id()
    ));
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

fn proposal(name: &str, prompt: &str) -> wuffagent_core::memory::NewAgentProposal {
    wuffagent_core::memory::NewAgentProposal {
        name: name.to_string(),
        description: "a helper".to_string(),
        system_prompt: prompt.to_string(),
        allowed_tools: vec!["file_io".to_string()],
    }
}

fn suggestion(agent: &str, rationale: &str, prompt: &str) -> wuffagent_core::memory::ImprovementSuggestion {
    wuffagent_core::memory::ImprovementSuggestion {
        agent_name: agent.to_string(),
        prompt_change: Some(prompt.to_string()),
        rationale: rationale.to_string(),
        new_agents: vec![],
        allowed_tools: None,
        reasoning_effort: None,
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: None,
        evidence: vec![],
    }
}

fn pending(agent: &str, prompt: Option<&str>) -> PendingImprovement {
    PendingImprovement {
        agent_name: agent.to_string(),
        prompt_change: prompt.map(str::to_string),
        edited_prompt: prompt.map(str::to_string),
        rationale: "test".to_string(),
        new_agents: vec![],
        revert_armed: false,
        allowed_tools: None,
        reasoning_effort: None,
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: None,
        apply_prompt: true,
        apply_allowed_tools: true,
        apply_reasoning_effort: true,
        apply_shell_config: true,
        apply_handoff_targets: true,
        apply_task_timeout: true,
        evidence: vec![],
    }
}

#[test]
fn test_apply_improvement_updates_prompt_and_creates_agent() {
    let dir = temp_agents_dir("update");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let mut imp = pending("coder", Some("new prompt"));
    imp.new_agents.push(PendingNewAgent {
        proposal: proposal("helper", "helper prompt"),
        edited_system_prompt: "helper prompt".to_string(),
    });

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &imp).0;
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

/// F1: user edits made in the panel's text boxes (stored in `edited_prompt`
/// / `edited_system_prompt`) must be what gets persisted on approve, not the
/// original LLM text.
#[test]
fn test_apply_improvement_uses_edited_prompts() {
    let dir = temp_agents_dir("edited");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let mut imp = pending("coder", Some("llm original prompt"));
    imp.edited_prompt = Some("user edited prompt".to_string());
    imp.new_agents.push(PendingNewAgent {
        proposal: proposal("helper", "llm original helper prompt"),
        edited_system_prompt: "user edited helper prompt".to_string(),
    });

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &imp).0;
    assert!(msg.contains("updated prompt for 'coder'"), "msg: {}", msg);
    assert!(msg.contains("created new agent 'helper'"), "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.system_prompt, "user edited prompt");
    let helper = manager.get_agent("helper").expect("helper agent");
    assert_eq!(helper.system_prompt, "user edited helper prompt");

    let _ = std::fs::remove_dir_all(&dir);
}

/// F1 fallback: when `edited_prompt` was never set (e.g. a caller constructed
/// the struct manually), the original suggestion text is still applied.
#[test]
fn test_apply_improvement_falls_back_to_original_prompt() {
    let dir = temp_agents_dir("fallback");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let mut imp = pending("coder", Some("original only"));
    imp.edited_prompt = None;

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &imp).0;
    assert!(msg.contains("updated prompt for 'coder'"), "msg: {}", msg);
    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.system_prompt, "original only");

    let _ = std::fs::remove_dir_all(&dir);
}

/// F3: when the profile lives in NONE of the known agents directories
/// (deleted, or synthetic with no backing file), approve reports a clear
/// error naming the searched directories instead of a bare "not found".
#[test]
fn test_apply_improvement_missing_profile_clear_error() {
    let dir = temp_agents_dir("missing");
    let manager = AgentManager::new(dir.clone());

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &pending("ghost", Some("p"))).0;
    assert!(
        msg.contains("profile 'ghost' not found in any agents directory"),
        "msg: {}",
        msg
    );
    assert!(msg.contains("nothing was written"), "msg: {}", msg);

    let _ = std::fs::remove_dir_all(&dir);
}

/// F3: a profile that lives in a SEARCH dir (not the primary) must be
/// updated IN PLACE in that dir — no shadow copy in the primary — and its
/// history snapshot must land next to its file.
#[test]
fn test_apply_improvement_writes_to_actual_profile_dir() {
    let primary = temp_agents_dir("f3-primary");
    let search = temp_agents_dir("f3-search");

    // Profile only in the search dir.
    let mgr_search = AgentManager::new(search.clone());
    mgr_search.add_agent(&existing_agent("coder")).unwrap();

    // Manager as the panel builds it: primary first, search dir after.
    let mut manager = AgentManager::new(primary.clone());
    manager.add_search_dir(search.clone());

    let dirs = vec![primary.clone(), search.clone()];
    let msg = apply_improvement_detailed(&dirs, &manager, &pending("coder", Some("new prompt"))).0;
    assert!(msg.contains("updated prompt for 'coder'"), "msg: {}", msg);

    // The search-dir file was updated in place...
    let coder = mgr_search.get_agent("coder").expect("coder in search dir");
    assert_eq!(coder.system_prompt, "new prompt");
    // ...and the primary dir got NO shadow copy...
    assert!(!primary.join("coder.json").exists(), "no shadow copy in primary");
    // ...and the snapshot landed next to the profile.
    assert_eq!(
        mgr_search.list_agent_history("coder").unwrap().len(),
        1,
        "snapshot next to the profile"
    );

    let _ = std::fs::remove_dir_all(&primary);
    let _ = std::fs::remove_dir_all(&search);
}

/// F3: `resolve_agent_dir` picks the FIRST directory (priority order) that
/// actually holds the profile, and finds legacy `WorkerConfig` files too.
#[test]
fn test_resolve_agent_dir_priority_and_legacy() {
    let primary = temp_agents_dir("res-primary");
    let search = temp_agents_dir("res-search");
    let dirs = vec![primary.clone(), search.clone()];

    // Only in search dir.
    AgentManager::new(search.clone())
        .add_agent(&existing_agent("only-search"))
        .unwrap();
    assert_eq!(resolve_agent_dir(&dirs, "only-search"), Some(search.clone()));

    // In both: primary wins.
    AgentManager::new(primary.clone())
        .add_agent(&existing_agent("both"))
        .unwrap();
    AgentManager::new(search.clone())
        .add_agent(&existing_agent("both"))
        .unwrap();
    assert_eq!(resolve_agent_dir(&dirs, "both"), Some(primary.clone()));

    // Legacy WorkerConfig file (name field is what matches).
    let legacy = wuffagent_core::agents::config::WorkerConfig {
        name: "legacy-agent".to_string(),
        description: "old format".to_string(),
        system_prompt: "legacy prompt".to_string(),
        ..Default::default()
    };
    legacy
        .save_to_file(&search.join("legacy-agent.json"))
        .unwrap();
    assert_eq!(
        resolve_agent_dir(&dirs, "legacy-agent"),
        Some(search.clone())
    );

    // Unknown.
    assert_eq!(resolve_agent_dir(&dirs, "nope"), None);

    let _ = std::fs::remove_dir_all(&primary);
    let _ = std::fs::remove_dir_all(&search);
}

/// I2: a field-only suggestion (no prompt change) applies just the proposed
/// fields, and a toggled-off field is left untouched.
#[test]
fn test_apply_improvement_field_only_and_toggles() {
    let dir = temp_agents_dir("fields");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let mut imp = pending("coder", None); // no prompt change
    imp.allowed_tools = Some(vec!["file_io".to_string(), "shell".to_string()]);
    imp.reasoning_effort = Some(ReasoningEffort::High);
    imp.apply_reasoning_effort = false; // user rejects the reasoning change

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &imp).0;
    assert!(msg.contains("updated tools for 'coder'"), "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.allowed_tools, vec!["file_io", "shell"]);
    assert_eq!(
        coder.reasoning_effort,
        ReasoningEffort::default(),
        "toggled-off field must stay untouched"
    );
    assert_eq!(coder.system_prompt, "old prompt", "no prompt change proposed");

    let _ = std::fs::remove_dir_all(&dir);
}

/// I2/I3: a toggled-off PROMPT is skipped while the toggled-on field is
/// applied — "accept the tool change but reject the prompt change".
#[test]
fn test_apply_improvement_prompt_off_fields_on() {
    let dir = temp_agents_dir("prompt-off");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let mut imp = pending("coder", Some("new prompt"));
    imp.apply_prompt = false;
    imp.task_timeout_ms = Some(120_000);

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &imp).0;
    assert!(msg.contains("updated timeout for 'coder'"), "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.system_prompt, "old prompt", "prompt change was rejected");
    assert_eq!(coder.task_timeout_ms, 120_000);

    let _ = std::fs::remove_dir_all(&dir);
}

/// I2: a suggestion with every field toggled off is a no-op ("No changes to
/// apply") and writes nothing.
#[test]
fn test_apply_improvement_all_toggles_off_is_noop() {
    let dir = temp_agents_dir("toggles-off");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();

    let mut imp = pending("coder", Some("new prompt"));
    imp.apply_prompt = false;
    imp.allowed_tools = Some(vec!["file_io".to_string()]);
    imp.apply_allowed_tools = false;

    let msg = apply_improvement_detailed(&[dir.clone()], &manager, &imp).0;
    assert_eq!(msg, "No changes to apply.", "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.system_prompt, "old prompt");

    let _ = std::fs::remove_dir_all(&dir);
}

/// F2: a new suggestion batch must not drop previously unreviewed entries.
#[test]
fn test_new_batch_preserves_unreviewed_suggestions() {
    let mut panel = ImprovementsPanel::new();
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "first", "p1")]);
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "second", "p2")]);

    assert_eq!(panel.pending.len(), 2);
    assert_eq!(panel.pending[0].rationale, "first");
    assert_eq!(panel.pending[1].rationale, "second");
    assert!(panel.show_panel);
}

/// F2: a duplicate suggestion (same agent_name + rationale) replaces the
/// existing pending entry instead of appending a second copy.
#[test]
fn test_duplicate_suggestion_replaces_existing() {
    let mut panel = ImprovementsPanel::new();
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "same", "old prompt")]);
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "same", "new prompt")]);

    assert_eq!(panel.pending.len(), 1);
    assert_eq!(panel.pending[0].prompt_change.as_deref(), Some("new prompt"));
    // The edit buffer is re-initialized from the replacement's LLM text.
    assert_eq!(panel.pending[0].edited_prompt.as_deref(), Some("new prompt"));
    // Re-arming state resets with the replacement.
    panel.pending[0].revert_armed = true;
    panel
        .handle_improvement_suggested("coder", vec![suggestion("coder", "same", "newer prompt")]);
    assert!(!panel.pending[0].revert_armed);

    // Same rationale for a DIFFERENT agent is not a duplicate.
    panel.handle_improvement_suggested("coder", vec![suggestion("researcher", "same", "p3")]);
    assert_eq!(panel.pending.len(), 2);
}

/// F5: the rejection lesson must be a Lesson entry that names the agent in
/// its content (so `collect_lessons`'s agent-name keyword search finds it)
/// and carries the `improvement-rejected` + `agent:<name>` tags.
#[test]
fn test_rejection_lesson_shape() {
    let mut imp = pending("coder", Some("new prompt"));
    imp.rationale = "too verbose".to_string();

    let entry = rejection_lesson(&imp);
    assert!(matches!(entry.r#type, wuffagent_core::memory::MemoryType::Lesson));
    assert_eq!(entry.source, "improvement-review");
    assert!(
        entry.tags.contains(&"improvement-rejected".to_string()),
        "tags: {:?}",
        entry.tags
    );
    assert!(
        entry.tags.contains(&"agent:coder".to_string()),
        "tags: {:?}",
        entry.tags
    );
    // Names the agent + the rejected change + the "do not re-suggest" hint.
    assert!(entry.content.contains("coder"), "content: {}", entry.content);
    assert!(entry.content.contains("too verbose"), "content: {}", entry.content);
    assert!(
        entry.content.contains("do not re-suggest"),
        "content: {}",
        entry.content
    );
}

/// F5: dismissing persists a lesson through the shared save path, a repeated
/// dismissal is collapsed by the dedup gate, and the stored lesson is
/// retrievable by an agent-name search (what `collect_lessons` does).
#[test]
fn test_remember_dismissal_saves_and_dedups() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-egui-f5-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = wuffagent_core::memory::MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = wuffagent_core::memory::MemoryManager::new(config).unwrap();

    let imp = pending("coder", Some("new prompt"));
    assert!(remember_dismissal(&manager, &imp).is_ok());
    // Same suggestion dismissed again (e.g. the LLM re-suggested it) ->
    // dedup gate, still Ok, no second entry.
    assert!(remember_dismissal(&manager, &imp).is_ok());
    assert_eq!(manager.count(), 1, "dedup must collapse repeats");

    // collect_lessons searches by agent name + task text: the lesson content
    // contains the agent name, so a plain agent-name search finds it.
    let found = manager.search("coder");
    assert!(
        found.iter().any(|l| l.content.contains("do not re-suggest")),
        "agent-name search must find the rejection lesson"
    );

    // A dismissal of a DIFFERENT change (distinct rationale) is a distinct
    // entry — near-identical rationales would be collapsed by the store's
    // fuzzy dedup gate, which is intended (same change, re-dismissed).
    let mut other = pending("researcher", Some("p"));
    other.rationale = "simplify the handoff protocol instead".to_string();
    assert!(remember_dismissal(&manager, &other).is_ok());
    assert_eq!(manager.count(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// I5: the approval marker must be a Fact entry (NOT a Lesson — so the
/// `collect_lessons` Lesson filter ignores it: a reference point, not
/// evidence), carrying the `improvement-applied` + `agent:<name>` tags and
/// naming the agent and the approval date in its content (the date keeps
/// re-approvals on different days from being collapsed by the dedup gate).
#[test]
fn test_applied_marker_shape() {
    let imp = pending("coder", Some("new prompt"));

    let entry = applied_marker(&imp);
    assert!(matches!(entry.r#type, wuffagent_core::memory::MemoryType::Fact));
    assert_eq!(entry.source, "improvement-review");
    assert!(
        entry.tags.contains(&"improvement-applied".to_string()),
        "tags: {:?}",
        entry.tags
    );
    assert!(
        entry.tags.contains(&"agent:coder".to_string()),
        "tags: {:?}",
        entry.tags
    );
    // Names the agent and the approval date (tolerate a midnight rollover
    // between building the marker and computing "today").
    assert!(entry.content.contains("coder"), "content: {}", entry.content);
    let now = chrono::Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let yesterday = (now - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
    assert!(
        entry.content.contains(&today) || entry.content.contains(&yesterday),
        "content: {} (today: {})",
        entry.content,
        today
    );
}

/// I5: an approval persists through the shared save path (same as F5's
/// rejection lessons); a same-day re-approval of the same change is collapsed
/// by the dedup gate, and a different prompt is a distinct entry (the content
/// includes an excerpt of the applied prompt).
#[test]
fn test_remember_applied_prompt_saves_and_dedups() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-egui-i5-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = wuffagent_core::memory::MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = wuffagent_core::memory::MemoryManager::new(config).unwrap();

    let imp = pending("coder", Some("new prompt"));
    assert!(remember_applied_prompt(&manager, &imp).is_ok());
    // Same change approved again (same day) -> dedup gate, still Ok.
    assert!(remember_applied_prompt(&manager, &imp).is_ok());
    assert_eq!(manager.count(), 1, "dedup must collapse same-day repeats");

    // Findable by tag (what the improver's latest_applied_marker does).
    let markers = manager.get_by_tag("improvement-applied");
    assert_eq!(markers.len(), 1);
    assert!(markers[0].tags.contains(&"agent:coder".to_string()));

    // A DIFFERENT prompt for the same agent is a distinct entry.
    let other = pending(
        "coder",
        Some("a completely different prompt with lots of extra unique wording to diverge"),
    );
    assert!(remember_applied_prompt(&manager, &other).is_ok());
    assert_eq!(manager.count(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}
