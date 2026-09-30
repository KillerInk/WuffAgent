//! Unit tests for the `improvements` module (see `super`).

use super::*;

fn temp_agents_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent-egui-imp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A temp skills dir for tests (same scratch pattern as `temp_agents_dir`).
fn temp_skill_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-egui-skills-{tag}-{}",
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

fn suggestion(
    agent: &str,
    rationale: &str,
    prompt: &str,
) -> wuffagent_core::memory::ImprovementSuggestion {
    wuffagent_core::memory::ImprovementSuggestion {
        agent_name: agent.to_string(),
        prompt_change: Some(prompt.to_string()),
        rationale: rationale.to_string(),
        description: None,
        new_agents: vec![],
        allowed_tools: None,
        reasoning_effort: None,
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: None,
        skill_updates: vec![],
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
        skill_revert_armed: vec![],
        description: None,
        allowed_tools: None,
        reasoning_effort: None,
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: None,
        apply_prompt: true,
        apply_description: true,
        apply_allowed_tools: true,
        apply_reasoning_effort: true,
        apply_shell_config: true,
        apply_handoff_targets: true,
        apply_task_timeout: true,
        skill_updates: vec![],
        apply_skills: true,
        evidence: vec![],
    }
}

/// A temp app `Config` backed by a real (temp) `config.json` path, so
/// `apply_chat_improvement`'s `config.save()` has somewhere to persist.
fn temp_config(tag: &str) -> (wuffagent_core::config::Config, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "wuffagent-egui-imp-{tag}-{}-config.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut cfg = wuffagent_core::config::Config::default();
    cfg.file_path = path.clone();
    cfg.system_prompt = "live chat prompt".to_string();
    cfg.save().unwrap();
    (cfg, path)
}

/// The synthetic chat profile (no backing .json file) is approved through the
/// APP CONFIG, not an agents dir: the prompt lands in `config.json` (persisted),
/// bundled new agents are still created, profile-field changes are reported as
/// skipped, and the result reports `prompt_applied` like the file-backed path.
#[test]
fn test_apply_chat_improvement_writes_config() {
    let dir = temp_agents_dir("chat");
    let manager = AgentManager::new(dir.clone());
    let (mut config, cfg_path) = temp_config("chat");

    let mut imp = pending("chat", Some("llm chat prompt"));
    imp.edited_prompt = Some("user edited chat prompt".to_string());
    imp.new_agents.push(PendingNewAgent {
        proposal: proposal("helper", "helper prompt"),
        edited_system_prompt: "helper prompt".to_string(),
    });
    imp.allowed_tools = Some(vec!["shell".to_string()]);

    let (msg, prompt_applied) =
        apply_chat_improvement(&mut config, &manager, &SkillStore::new(temp_skill_dir("chat")), &imp);
    assert!(prompt_applied, "chat prompt write must report prompt_applied: {msg}");
    assert!(msg.contains("updated prompt for the chat profile in config.json"), "msg: {msg}");
    assert!(msg.contains("created new agent 'helper'"), "msg: {msg}");
    assert!(msg.contains("skipped chat-profile field change(s) tools"), "msg: {msg}");

    // The user-edited prompt (not the LLM text) is in the in-memory config
    // AND persisted to config.json.
    assert_eq!(config.system_prompt, "user edited chat prompt");
    let on_disk = wuffagent_core::config::Config::load(&cfg_path).unwrap();
    assert_eq!(on_disk.system_prompt, "user edited chat prompt");

    // Bundled new agents still go through the agent files.
    assert_eq!(manager.get_agent("helper").expect("helper").system_prompt, "helper prompt");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&cfg_path);
}

/// Approving a chat suggestion whose prompt is already current is a no-op
/// success (prompt_applied, "unchanged").
#[test]
fn test_apply_chat_improvement_prompt_unchanged() {
    let dir = temp_agents_dir("chat-same");
    let manager = AgentManager::new(dir.clone());
    let (mut config, cfg_path) = temp_config("chat-same");

    let imp = pending("chat", Some("live chat prompt"));
    let (msg, prompt_applied) = apply_chat_improvement(
        &mut config,
        &manager,
        &SkillStore::new(temp_skill_dir("chat-same")),
        &imp,
    );
    assert!(prompt_applied, "msg: {msg}");
    assert!(msg.contains("chat prompt unchanged"), "msg: {msg}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&cfg_path);
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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t1")),
        &imp,
    )
    .0;
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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t2")),
        &imp,
    )
    .0;
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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t3")),
        &imp,
    )
    .0;
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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t4")),
        &pending("ghost", Some("p")),
    )
    .0;
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
    let msg = apply_improvement_detailed(
        &dirs,
        &manager,
        &SkillStore::new(temp_skill_dir("t5")),
        &pending("coder", Some("new prompt")),
    )
    .0;
    assert!(msg.contains("updated prompt for 'coder'"), "msg: {}", msg);

    // The search-dir file was updated in place...
    let coder = mgr_search.get_agent("coder").expect("coder in search dir");
    assert_eq!(coder.system_prompt, "new prompt");
    // ...and the primary dir got NO shadow copy...
    assert!(
        !primary.join("coder.json").exists(),
        "no shadow copy in primary"
    );
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
    assert_eq!(
        resolve_agent_dir(&dirs, "only-search"),
        Some(search.clone())
    );

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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t6")),
        &imp,
    )
    .0;
    assert!(msg.contains("updated tools for 'coder'"), "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.allowed_tools, vec!["file_io", "shell"]);
    assert_eq!(
        coder.reasoning_effort,
        ReasoningEffort::default(),
        "toggled-off field must stay untouched"
    );
    assert_eq!(
        coder.system_prompt, "old prompt",
        "no prompt change proposed"
    );

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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t7")),
        &imp,
    )
    .0;
    assert!(msg.contains("updated timeout for 'coder'"), "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(
        coder.system_prompt, "old prompt",
        "prompt change was rejected"
    );
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

    let msg = apply_improvement_detailed(
        &[dir.clone()],
        &manager,
        &SkillStore::new(temp_skill_dir("t8")),
        &imp,
    )
    .0;
    assert_eq!(msg, "No changes to apply.", "msg: {}", msg);

    let coder = manager.get_agent("coder").expect("coder agent");
    assert_eq!(coder.system_prompt, "old prompt");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A panel whose persistence store is a scratch file (NOT the real
/// `~/.wuffagent/pending_improvements.json`). `handle_improvement_suggested`
/// persists on arrival, so a default store would leak the test fixtures into
/// the live app's pending queue — visible after the next app start as bogus
/// pending suggestions ("newer prompt" for coder, "p3" for researcher).
fn panel_with_temp_store(tag: &str) -> ImprovementsPanel {
    let mut panel = ImprovementsPanel::new();
    panel.store = PendingStore::at(pending_file(tag));
    panel
}

/// F2: a new suggestion batch must not drop previously unreviewed entries.
#[test]
fn test_new_batch_preserves_unreviewed_suggestions() {
    let mut panel = panel_with_temp_store("f2-batch");
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "first", "p1")]);
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "second", "p2")]);

    assert_eq!(panel.pending.len(), 2);
    assert_eq!(panel.pending[0].rationale, "first");
    assert_eq!(panel.pending[1].rationale, "second");
    assert!(panel.show_panel);
    let _ = std::fs::remove_file(panel.store.path());
}

/// F2: a duplicate suggestion (same agent_name + rationale) replaces the
/// existing pending entry instead of appending a second copy.
#[test]
fn test_duplicate_suggestion_replaces_existing() {
    let mut panel = panel_with_temp_store("f2-dup");
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "same", "old prompt")]);
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "same", "new prompt")]);

    assert_eq!(panel.pending.len(), 1);
    assert_eq!(
        panel.pending[0].prompt_change.as_deref(),
        Some("new prompt")
    );
    // The edit buffer is re-initialized from the replacement's LLM text.
    assert_eq!(
        panel.pending[0].edited_prompt.as_deref(),
        Some("new prompt")
    );
    // Re-arming state resets with the replacement (agent + per-skill, 3c).
    panel.pending[0].revert_armed = true;
    panel.pending[0].skill_revert_armed = vec!["some-skill".to_string()];
    panel.handle_improvement_suggested("coder", vec![suggestion("coder", "same", "newer prompt")]);
    assert!(!panel.pending[0].revert_armed);
    assert!(panel.pending[0].skill_revert_armed.is_empty());

    // Same rationale for a DIFFERENT agent is not a duplicate.
    panel.handle_improvement_suggested("coder", vec![suggestion("researcher", "same", "p3")]);
    assert_eq!(panel.pending.len(), 2);
    let _ = std::fs::remove_file(panel.store.path());
}

/// F5: the rejection lesson must be a Lesson entry that names the agent in
/// its content (so `collect_lessons`'s agent-name keyword search finds it)
/// and carries the `improvement-rejected` + `agent:<name>` tags.
#[test]
fn test_rejection_lesson_shape() {
    let mut imp = pending("coder", Some("new prompt"));
    imp.rationale = "too verbose".to_string();

    let entry = rejection_lesson(&imp);
    assert!(matches!(
        entry.r#type,
        wuffagent_core::memory::MemoryType::Lesson
    ));
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
    assert!(
        entry.content.contains("coder"),
        "content: {}",
        entry.content
    );
    assert!(
        entry.content.contains("too verbose"),
        "content: {}",
        entry.content
    );
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
    let dir = std::env::temp_dir().join(format!("wuffagent-egui-f5-{}", std::process::id()));
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
        found
            .iter()
            .any(|l| l.content.contains("do not re-suggest")),
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
    assert!(matches!(
        entry.r#type,
        wuffagent_core::memory::MemoryType::Fact
    ));
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
    assert!(
        entry.content.contains("coder"),
        "content: {}",
        entry.content
    );
    let now = chrono::Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let yesterday = (now - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
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
    let dir = std::env::temp_dir().join(format!("wuffagent-egui-i5-{}", std::process::id()));
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

/// 3c: approved skill updates are written to the skill store; invalid names
/// are reported and skipped; `apply_skills: false` writes nothing; the
/// profile itself is untouched when no prompt was proposed.
#[test]
fn test_apply_improvement_skill_updates() {
    let dir = temp_agents_dir("skills");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();
    let skills_root = temp_skill_dir("skills-store");
    let store = SkillStore::new(skills_root.clone());

    let mut imp = pending("coder", None); // profile-only context, no prompt change
    imp.skill_updates = vec![
        SkillUpdate {
            name: "good-skill".to_string(),
            action: "new".to_string(),
            description: "a useful procedure".to_string(),
            when_to_use: "when the task matches".to_string(),
            body: "1. do the thing".to_string(),
        },
        SkillUpdate {
            name: "Bad_Name".to_string(), // invalid slug (underscore + uppercase)
            action: "new".to_string(),
            description: "d".to_string(),
            when_to_use: "w".to_string(),
            body: "x".to_string(),
        },
    ];

    let (msg, prompt_applied) = apply_improvement_detailed(&[dir.clone()], &manager, &store, &imp);
    assert!(!prompt_applied, "no prompt proposed, msg: {msg}");
    assert!(msg.contains("saved skill 'good-skill'"), "msg: {msg}");
    assert!(msg.contains("skipped skill 'Bad_Name'"), "msg: {msg}");

    // The good skill is in the store with its frontmatter...
    let saved = store.read("good-skill").expect("skill saved");
    assert_eq!(saved.description, "a useful procedure");
    // ...and the profile itself was not touched (no prompt proposed).
    assert_eq!(
        manager.get_agent("coder").unwrap().system_prompt,
        "old prompt"
    );

    // Batch toggle: apply_skills = false writes nothing.
    let mut off = pending("coder", None);
    off.apply_skills = false;
    off.skill_updates = vec![SkillUpdate {
        name: "never-saved".to_string(),
        action: "new".to_string(),
        description: "d".to_string(),
        when_to_use: "w".to_string(),
        body: "x".to_string(),
    }];
    let (msg_off, _) = apply_improvement_detailed(&[dir.clone()], &manager, &store, &off);
    assert_eq!(msg_off, "No changes to apply.", "msg: {msg_off}");
    assert!(
        store.read("never-saved").is_none(),
        "apply_skills=false skips the save"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&skills_root);
}

/// 3b: `action: "delete"` retires the skill file (an empty body is fine,
/// which `save` would reject); deleting a missing skill is a noted no-op;
/// the action is case-insensitive; mixed batches (delete + update) work.
#[test]
fn test_apply_improvement_skill_delete() {
    let dir = temp_agents_dir("skill-del");
    let manager = AgentManager::new(dir.clone());
    manager.add_agent(&existing_agent("coder")).unwrap();
    let skills_root = temp_skill_dir("skills-del-store");
    let store = SkillStore::new(skills_root.clone());
    store
        .save("stale-skill", "old", "rare", "1. old steps")
        .unwrap();
    store
        .save("keep-skill", "good", "often", "1. old steps")
        .unwrap();

    let mut imp = pending("coder", None);
    imp.skill_updates = vec![
        SkillUpdate {
            name: "stale-skill".to_string(),
            action: "delete".to_string(),
            description: String::new(),
            when_to_use: String::new(),
            body: String::new(),
        },
        SkillUpdate {
            name: "ghost-skill".to_string(),
            action: "DELETE".to_string(), // case-insensitive; skill is missing
            description: String::new(),
            when_to_use: String::new(),
            body: String::new(),
        },
        SkillUpdate {
            name: "keep-skill".to_string(),
            action: "update".to_string(),
            description: "good".to_string(),
            when_to_use: "often".to_string(),
            body: "1. new steps".to_string(),
        },
    ];

    let (msg, prompt_applied) = apply_improvement_detailed(&[dir.clone()], &manager, &store, &imp);
    assert!(!prompt_applied, "no prompt proposed, msg: {msg}");
    assert!(msg.contains("retired skill 'stale-skill'"), "msg: {msg}");
    assert!(msg.contains("already gone"), "msg: {msg}");
    assert!(msg.contains("saved skill 'keep-skill'"), "msg: {msg}");

    assert!(
        store.read("stale-skill").is_none(),
        "delete removed the file"
    );
    let kept = store.read("keep-skill").expect("keep-skill still there");
    assert_eq!(kept.body, "1. new steps");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&skills_root);
}

/// 3c: the panel's per-skill Revert flow — approve a skill update (which
/// snapshots the previous version), then revert to the latest snapshot,
/// exactly the sequence the button executes (list → latest → revert). The
/// original content comes back and the revert stays reversible.
#[test]
fn test_skill_revert_flow_after_approved_update() {
    let skills_root = temp_skill_dir("skills-revert-store");
    let store = SkillStore::new(skills_root.clone());
    store
        .save("my-skill", "old desc", "old when", "1. original steps")
        .unwrap();

    // Approve an update (what the panel's Approve does for skills).
    let mut imp = pending("coder", None);
    imp.skill_updates = vec![SkillUpdate {
        name: "my-skill".to_string(),
        action: "update".to_string(),
        description: "new desc".to_string(),
        when_to_use: "new when".to_string(),
        body: "1. rewritten steps".to_string(),
    }];
    let (msg, prompt_applied) = apply_improvement_detailed(
        &[],
        &AgentManager::new(temp_agents_dir("skill-revert-dir")),
        &store,
        &imp,
    );
    assert!(!prompt_applied);
    assert!(msg.contains("saved skill 'my-skill'"), "msg: {msg}");
    assert_eq!(store.read("my-skill").unwrap().body, "1. rewritten steps");

    // Panel Revert: list → latest snapshot → revert.
    let hist = store.list_skill_history("my-skill");
    assert_eq!(hist.len(), 1, "the approved update snapshotted v1");
    let restored = store.revert_skill("my-skill", &hist[0]).expect("revert");
    assert_eq!(
        restored.body, "1. original steps",
        "original content is back"
    );
    assert_eq!(store.read("my-skill").unwrap().body, "1. original steps");
    // Reversible: the pre-revert (rewritten) state was snapshotted too.
    let hist2 = store.list_skill_history("my-skill");
    assert_eq!(hist2.len(), 2);
    let forward = store
        .revert_skill("my-skill", &hist2[0])
        .expect("forward again");
    assert_eq!(forward.body, "1. rewritten steps");

    let _ = std::fs::remove_dir_all(&skills_root);
}

// ── G.1: pending queue persistence ───────────────────────────────────────

/// A scratch file path for pending-queue tests (fresh per tag).
fn pending_file(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-egui-pending-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pending.json");
    let _ = std::fs::remove_file(&path);
    path
}

fn test_suggestion(prompt: &str) -> ImprovementSuggestion {
    ImprovementSuggestion {
        agent_name: "coder".to_string(),
        prompt_change: Some(prompt.to_string()),
        rationale: "too many shell errors".to_string(),
        new_agents: vec![],
        description: None,
        allowed_tools: None,
        reasoning_effort: None,
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: None,
        skill_updates: vec![],
        evidence: vec!["metrics: 3 errors".to_string()],
    }
}

#[test]
fn pending_queue_survives_panel_rebuild() {
    let path = pending_file("roundtrip");
    let mut panel = ImprovementsPanel::new();
    panel.store = PendingStore::at(path.clone());
    panel.handle_improvement_suggested("coder", vec![test_suggestion("proposed prompt")]);
    assert!(path.exists(), "the batch must be persisted on arrival");

    // A new app (fresh panel) restores the queue and opens the panel.
    let mut restored = ImprovementsPanel::new();
    restored.store = PendingStore::at(path.clone());
    restored.load_pending();
    assert_eq!(restored.pending.len(), 1);
    assert_eq!(restored.pending[0].agent_name, "coder");
    assert_eq!(
        restored.pending[0].prompt_change.as_deref(),
        Some("proposed prompt")
    );
    assert_eq!(restored.pending[0].rationale, "too many shell errors");
    assert!(restored.show_panel, "restored queue re-opens the panel");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn user_edits_are_the_persisted_prompt() {
    let path = pending_file("edit");
    let mut panel = ImprovementsPanel::new();
    panel.store = PendingStore::at(path.clone());
    panel.handle_improvement_suggested("coder", vec![test_suggestion("original")]);
    // The user edits the proposed prompt in the panel (F1).
    panel.pending[0].edited_prompt = Some("user's edited version".to_string());
    panel.persist();

    let mut restored = ImprovementsPanel::new();
    restored.store = PendingStore::at(path.clone());
    restored.load_pending();
    assert_eq!(
        restored.pending[0].prompt_change.as_deref(),
        Some("user's edited version"),
        "the user's edit wins over the LLM's original on reload"
    );
    assert_eq!(
        restored.pending[0].edited_prompt.as_deref(),
        Some("user's edited version")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_queue_removes_the_file() {
    let path = pending_file("empty");
    let mut panel = ImprovementsPanel::new();
    panel.store = PendingStore::at(path.clone());
    panel.handle_improvement_suggested("coder", vec![test_suggestion("p")]);
    assert!(path.exists());
    // Everything reviewed → the file goes away (no empty doc left behind).
    panel.pending.clear();
    panel.persist();
    assert!(!path.exists());
    let _ = std::fs::remove_file(&path);
}

// ── 4b: "run check now" button state ──────────────────────────────────────

/// 4b: `mark_check_finished` (the `ImprovementCheckFinished` handler) clears
/// the running state and records a status line that reflects whether the
/// on-demand check produced suggestions.
#[test]
fn test_mark_check_finished_sets_status() {
    let mut panel = ImprovementsPanel::new();
    panel.run_check_running = true;
    panel.run_check_status = String::new();

    panel.mark_check_finished("coder", true);
    assert!(!panel.run_check_running, "running flag must clear");
    assert!(
        panel.run_check_status.contains("coder"),
        "status: {}",
        panel.run_check_status
    );
    assert!(
        panel.run_check_status.contains("suggestions added"),
        "produced=true must say suggestions were added, status: {}",
        panel.run_check_status
    );

    panel.mark_check_finished("coder", false);
    assert!(!panel.run_check_running);
    assert!(
        panel.run_check_status.contains("no suggestions"),
        "produced=false must say no suggestions, status: {}",
        panel.run_check_status
    );
}

// ── Agent list cache (the per-frame list_agents() log-flood fix) ──────────

/// The agent list is cached: while no agent config is mutated (the
/// process-wide mutation counter is unchanged) and the dir set is the same,
/// a refresh must NOT re-read the dirs (that per-frame re-scan + per-agent
/// logging flooded the log while tokens streamed). Proof without log
/// capture: an agent FILE rewritten between refreshes is invisible to the
/// cache; a mutation (counter bump) makes the re-scan pick it up.
#[test]
fn agent_list_cache_skips_rescan_until_mutation() {
    let dir = temp_agents_dir("cache");
    AgentManager::new(dir.clone())
        .add_agent(&existing_agent("coder"))
        .unwrap();
    let mut panel = ImprovementsPanel::new();
    let manager = AgentManager::new(dir.clone());

    panel.refresh_agent_cache(&manager);
    assert_eq!(panel.agents().len(), 1, "first refresh populates the cache");
    assert_eq!(panel.agents()[0].system_prompt, "old prompt");

    // External in-place rewrite WITHOUT a mutation method → the next
    // refresh must still serve the cache (no rescan).
    let mut changed = existing_agent("coder");
    changed.system_prompt = "changed on disk".to_string();
    changed.save_to_file(&dir.join("coder.json")).unwrap();
    let fp_before = panel
        .agent_cache
        .as_ref()
        .expect("cache populated by the first refresh")
        .fingerprint
        .clone();
    panel.refresh_agent_cache(&manager);
    let fp_after = panel
        .agent_cache
        .as_ref()
        .expect("cache populated")
        .fingerprint
        .clone();
    if fp_after == fp_before {
        // No mutation ran process-wide in the window: the refresh must have
        // skipped the re-scan and served the cache.
        assert_eq!(
            panel.agents()[0].system_prompt,
            "old prompt",
            "no mutation: refresh must skip the re-scan and serve the cache"
        );
    } else {
        // A CONCURRENT test in this process bumped the shared mutation
        // counter (tests run in parallel): the re-scan is legitimate and
        // must then see the rewritten file.
        assert_eq!(
            panel.agents()[0].system_prompt,
            "changed on disk",
            "mutation during the window: the re-scan must see the edit"
        );
    }

    // A mutation (via a manager method) bumps the counter → rescan.
    let mut edited = existing_agent("coder");
    edited.system_prompt = "edited via manager".to_string();
    manager.edit_agent("coder", &edited).unwrap();
    panel.refresh_agent_cache(&manager);
    assert_eq!(
        panel.agents()[0].system_prompt,
        "edited via manager",
        "mutation: the re-scan must see the edit"
    );
    let _ = changed;

    let _ = std::fs::remove_dir_all(&dir);
}
