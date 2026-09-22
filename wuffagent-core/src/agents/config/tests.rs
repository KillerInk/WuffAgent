//! Unit tests for the `config` module (see `super`).

use super::*;

#[test]
fn test_worker_config_backwards_compat_personality() {
    let json = r#"{"name":"test","description":"Desc","personality":"You are a test worker.","allowed_tools":["file_io"],"priority":0,"max_concurrent":1}"#;
    let config: WorkerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.system_prompt, "You are a test worker.");
    assert_eq!(config.name, "test");
    assert!(!config.shell_config.shell_enabled);
}

#[test]
fn test_worker_config_shell_config() {
    let json = r#"{
        "name":"executor",
        "description":"Build and run",
        "allowed_tools":["shell"],
        "shell_config": {
            "shell_enabled": true,
            "allowed_commands": ["cargo build.*", "git.*"],
            "shell_type": "powershell",
            "shell_timeout_ms": 60000
        }
    }"#;
    let config: WorkerConfig = serde_json::from_str(json).unwrap();
    assert!(config.shell_config.shell_enabled);
    assert_eq!(
        config.shell_config.allowed_commands,
        vec!["cargo build.*".to_string(), "git.*".to_string()]
    );
    assert_eq!(config.shell_config.shell_type, "powershell");
    assert_eq!(config.shell_config.shell_timeout_ms, 60000);
}

#[test]
fn test_worker_config_save_and_load() {
    let dir = std::env::temp_dir().join("wuffagent_test_agents");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let config = WorkerConfig {
        name: "test_agent".to_string(),
        description: "A test agent".to_string(),
        system_prompt: "You are a test agent.".to_string(),
        allowed_tools: vec!["file_io".to_string()],
        priority: 5,
        max_concurrent: 2,
        enabled: false,
        ..Default::default()
    };

    let path = dir.join("test_agent.json");
    config.save_to_file(&path).unwrap();

    let loaded = WorkerConfig::load_from_file(&path).unwrap();
    assert_eq!(loaded.name, "test_agent");
    assert_eq!(loaded.system_prompt, "You are a test agent.");
    assert_eq!(loaded.allowed_tools, vec!["file_io".to_string()]);
    assert_eq!(loaded.priority, 5);
    assert_eq!(loaded.max_concurrent, 2);
    assert!(!loaded.enabled);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_agent_manager_crud() {
    let dir = std::env::temp_dir().join("wuffagent_test_mgr");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mgr = AgentManager::new(dir.clone());

    // Add
    let config = AgentConfig {
        name: "mgr_test".to_string(),
        description: "Manager test".to_string(),
        system_prompt: "You are a mgr test.".to_string(),
        allowed_tools: vec!["file_io".to_string()],
        enabled: true,
        ..Default::default()
    };
    mgr.add_agent(&config).unwrap();
    assert!(mgr.get_agent("mgr_test").is_some());

    // List
    let agents = mgr.list_agents().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "mgr_test");

    // Edit
    let mut edited = config.clone();
    edited.description = "Updated description".to_string();
    mgr.edit_agent("mgr_test", &edited).unwrap();
    let loaded = mgr.get_agent("mgr_test").unwrap();
    assert_eq!(loaded.description, "Updated description");

    // Remove
    mgr.remove_agent("mgr_test").unwrap();
    assert!(mgr.get_agent("mgr_test").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_agent_manager_reload() {
    let dir = std::env::temp_dir().join("wuffagent_test_reload");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mgr = AgentManager::new(dir.clone());
    assert_eq!(mgr.reload().unwrap().len(), 0);

    let config = AgentConfig {
        name: "reload_test".to_string(),
        description: "Reload test".to_string(),
        system_prompt: "Reload prompt".to_string(),
        allowed_tools: vec![],
        enabled: true,
        ..Default::default()
    };
    mgr.add_agent(&config).unwrap();
    let loaded = mgr.reload().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "reload_test");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_agent_manager_multi_dir_search() {
    let primary_dir = std::env::temp_dir().join("wuffagent_test_primary");
    let search_dir = std::env::temp_dir().join("wuffagent_test_search");
    let _ = std::fs::remove_dir_all(&primary_dir);
    let _ = std::fs::remove_dir_all(&search_dir);
    std::fs::create_dir_all(&primary_dir).unwrap();
    std::fs::create_dir_all(&search_dir).unwrap();

    // Place an agent in the search dir (simulating project workers/)
    let search_agent = AgentConfig {
        name: "search_agent".to_string(),
        description: "From search dir".to_string(),
        system_prompt: "Search prompt".to_string(),
        allowed_tools: vec!["web_search".to_string()],
        enabled: true,
        ..Default::default()
    };
    search_agent
        .save_to_file(&search_dir.join("search_agent.json"))
        .unwrap();

    // Place an agent in the primary dir
    let primary_agent = AgentConfig {
        name: "primary_agent".to_string(),
        description: "From primary dir".to_string(),
        system_prompt: "Primary prompt".to_string(),
        allowed_tools: vec!["file_io".to_string()],
        enabled: true,
        ..Default::default()
    };
    primary_agent
        .save_to_file(&primary_dir.join("primary_agent.json"))
        .unwrap();

    // AgentManager with search dir
    let mut mgr = AgentManager::new(primary_dir.clone());
    mgr.add_search_dir(search_dir.clone());
    let agents = mgr.list_agents().unwrap();
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().any(|a| a.name == "primary_agent"));
    assert!(agents.iter().any(|a| a.name == "search_agent"));

    // Save should go to primary dir
    let new_agent = AgentConfig {
        name: "new_agent".to_string(),
        description: "New agent".to_string(),
        system_prompt: "New prompt".to_string(),
        allowed_tools: vec![],
        enabled: true,
        ..Default::default()
    };
    mgr.add_agent(&new_agent).unwrap();
    assert!(primary_dir.join("new_agent.json").exists());
    assert!(!search_dir.join("new_agent.json").exists());

    // Reload should find all 3
    let agents = mgr.reload().unwrap();
    assert_eq!(agents.len(), 3);

    let _ = std::fs::remove_dir_all(&primary_dir);
    let _ = std::fs::remove_dir_all(&search_dir);
}

#[test]
fn test_agent_config_handoff_fields() {
    let json = r#"{
        "name": "planner",
        "description": "Plans",
        "system_prompt": "Plan things.",
        "handoff_enabled": true,
        "handoff_targets": ["coder", "reviewer"]
    }"#;
    let config: AgentConfig = serde_json::from_str(json).unwrap();
    assert!(config.handoff_enabled);
    assert_eq!(config.handoff_targets, vec!["coder", "reviewer"]);
}

#[test]
fn test_agent_config_can_invoke_alias() {
    // Legacy files use `can_invoke`; it must map onto `handoff_targets`.
    let json = r#"{
        "name": "planner",
        "description": "Plans",
        "system_prompt": "Plan things.",
        "handoff_enabled": true,
        "can_invoke": ["coder"]
    }"#;
    let config: AgentConfig = serde_json::from_str(json).unwrap();
    assert!(config.handoff_enabled);
    assert_eq!(config.handoff_targets, vec!["coder"]);
}

#[test]
fn test_agent_config_handoff_defaults() {
    let json = r#"{"name": "plain", "system_prompt": "Be plain."}"#;
    let config: AgentConfig = serde_json::from_str(json).unwrap();
    assert!(!config.handoff_enabled);
    assert!(config.handoff_targets.is_empty());
}

#[test]
fn test_load_agent_from_dir() {
    let dir = std::env::temp_dir().join("wuffagent_test_handoff_agents");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Current-format agent, enabled, with handoff.
    std::fs::write(
        dir.join("planner.json"),
        r#"{"name":"planner","system_prompt":"Plan.","handoff_enabled":true,"handoff_targets":["coder"]}"#,
    )
    .unwrap();
    // Current-format agent, disabled.
    std::fs::write(
        dir.join("off.json"),
        r#"{"name":"off","system_prompt":"Off.","enabled":false}"#,
    )
    .unwrap();
    // Legacy WorkerConfig file using can_invoke + handoff_enabled.
    std::fs::write(
        dir.join("legacy.json"),
        r#"{"name":"legacy","description":"Legacy","personality":"You are legacy.","handoff_enabled":true,"can_invoke":["coder"]}"#,
    )
    .unwrap();
    // Non-JSON file must be skipped.
    std::fs::write(dir.join("notes.txt"), "not an agent").unwrap();

    let planner = load_agent_from_dir(&dir, "planner").unwrap();
    assert!(planner.handoff_enabled);
    assert_eq!(planner.handoff_targets, vec!["coder"]);
    assert!(planner.enabled);

    let legacy = load_agent_from_dir(&dir, "legacy").unwrap();
    assert_eq!(legacy.system_prompt, "You are legacy.");
    assert!(
        legacy.handoff_enabled,
        "legacy handoff_enabled must migrate"
    );
    assert_eq!(
        legacy.handoff_targets,
        vec!["coder"],
        "legacy can_invoke must migrate"
    );

    assert!(
        load_agent_from_dir(&dir, "off").is_none(),
        "disabled agent must not load"
    );
    assert!(load_agent_from_dir(&dir, "missing").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_load_agent_from_dirs_multi_dir_and_anchoring() {
    let primary = std::env::temp_dir().join("wuffagent_test_handoff_dirs_primary");
    let search_a = std::env::temp_dir().join("wuffagent_test_handoff_dirs_a");
    let search_b = std::env::temp_dir().join("wuffagent_test_handoff_dirs_b");
    for d in [&primary, &search_a, &search_b] {
        let _ = std::fs::remove_dir_all(d);
        std::fs::create_dir_all(d).unwrap();
    }

    // "coder" exists in BOTH primary and search_b; search_a has "helper".
    std::fs::write(
        primary.join("coder.json"),
        r#"{"name":"coder","system_prompt":"primary coder"}"#,
    )
    .unwrap();
    std::fs::write(
        search_a.join("helper.json"),
        r#"{"name":"helper","system_prompt":"helps"}"#,
    )
    .unwrap();
    std::fs::write(
        search_b.join("coder.json"),
        r#"{"name":"coder","system_prompt":"shadow coder"}"#,
    )
    .unwrap();

    let dirs = vec![primary.clone(), search_a.clone(), search_b.clone()];

    // First dir wins the dedup.
    let coder = load_agent_from_dirs(&dirs, "coder").unwrap();
    assert_eq!(coder.system_prompt, "primary coder");
    assert_eq!(coder.agents_dir, primary);

    // Found in the second dir: anchored there, remaining dirs (both
    // sides, original order) become the search dirs for chained handoffs.
    let helper = load_agent_from_dirs(&dirs, "helper").unwrap();
    assert_eq!(helper.agents_dir, search_a);
    assert_eq!(
        helper.agents_search_dirs,
        vec![primary.clone(), search_b.clone()]
    );

    assert!(load_agent_from_dirs(&dirs, "missing").is_none());

    for d in [&primary, &search_a, &search_b] {
        let _ = std::fs::remove_dir_all(d);
    }
}

// ─── Prompt history + rollback (F4) ────────────────────────────────────────

fn temp_agents_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("wuffagent_test_hist_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn hist_agent(name: &str, prompt: &str) -> AgentConfig {
    AgentConfig {
        name: name.to_string(),
        description: "history test".to_string(),
        system_prompt: prompt.to_string(),
        allowed_tools: vec![],
        enabled: true,
        ..Default::default()
    }
}

/// Editing an agent snapshots the PRE-edit file into `agents_dir/history/`.
#[test]
fn test_edit_agent_creates_history_snapshot() {
    let dir = temp_agents_dir("edit");
    let mgr = AgentManager::new(dir.clone());
    mgr.add_agent(&hist_agent("hist", "v1")).unwrap();
    // Adding does not snapshot: no history dir yet.
    assert!(!dir.join("history").exists());

    mgr.edit_agent("hist", &hist_agent("hist", "v2")).unwrap();

    let snaps = mgr.list_agent_history("hist").unwrap();
    assert_eq!(snaps.len(), 1);
    assert!(snaps[0].starts_with(dir.join("history")));
    // The snapshot holds the PRE-edit prompt.
    let content = std::fs::read_to_string(&snaps[0]).unwrap();
    assert!(
        content.contains("v1"),
        "snapshot should contain old prompt, got: {}",
        content
    );
    // The live file holds the new prompt.
    assert_eq!(mgr.get_agent("hist").unwrap().system_prompt, "v2");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Editing a non-existent agent file is a plain write: no history dir appears.
#[test]
fn test_edit_missing_agent_creates_no_history() {
    let dir = temp_agents_dir("missing");
    let mgr = AgentManager::new(dir.clone());
    mgr.edit_agent("missing", &hist_agent("missing", "v1"))
        .unwrap();
    assert!(!dir.join("history").exists());
    assert_eq!(mgr.get_agent("missing").unwrap().system_prompt, "v1");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Only the newest 20 snapshots per agent are kept (oldest pruned).
#[test]
fn test_history_capped_at_20() {
    let dir = temp_agents_dir("cap");
    let mgr = AgentManager::new(dir.clone());
    mgr.add_agent(&hist_agent("cap", "v0")).unwrap();

    // 25 edits → 25 snapshots taken, capped at 20 on the way.
    for i in 1..=25 {
        mgr.edit_agent("cap", &hist_agent("cap", &format!("v{}", i)))
            .unwrap();
    }

    let snaps = mgr.list_agent_history("cap").unwrap();
    assert_eq!(snaps.len(), AgentManager::HISTORY_SNAPSHOTS_KEEP);
    // The newest snapshot is the pre-last-edit state (v24); the oldest
    // retained is v5 (v0..v4 were pruned first).
    let newest = std::fs::read_to_string(&snaps[0]).unwrap();
    assert!(
        newest.contains("v24"),
        "newest snapshot should hold v24, got: {}",
        newest
    );
    let oldest = std::fs::read_to_string(&snaps[19]).unwrap();
    assert!(
        oldest.contains("v5"),
        "oldest kept snapshot should hold v5, got: {}",
        oldest
    );
    // Every retained snapshot is a distinct pre-edit state: exactly v5..v24.
    let mut prompts: Vec<String> = Vec::new();
    for p in &snaps {
        let c = std::fs::read_to_string(p).unwrap();
        prompts.push(
            c.split("\"system_prompt\": \"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("")
                .to_string(),
        );
    }
    prompts.sort();
    prompts.dedup();
    assert_eq!(
        prompts.len(),
        20,
        "retained snapshots must be distinct states"
    );
    assert!(
        !prompts
            .iter()
            .any(|p| matches!(p.as_str(), "v0" | "v1" | "v2" | "v3" | "v4")),
        "pruned early states leaked back: {:?}",
        prompts
    );
    // No overflow in the raw dir.
    let count = std::fs::read_dir(dir.join("history"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("cap-"))
        .count();
    assert_eq!(count, 20);

    let _ = std::fs::remove_dir_all(&dir);
}

/// `list_agent_history` returns snapshots newest-first, ignores other
/// agents' files, and yields an empty vec (not an error) for unknown agents.
#[test]
fn test_list_agent_history_newest_first_and_unknown_empty() {
    let dir = temp_agents_dir("list");
    let mgr = AgentManager::new(dir.clone());

    // Unknown agent: no history dir at all.
    assert!(mgr.list_agent_history("ghost").unwrap().is_empty());

    // Seed snapshots with distinct timestamps, out of insertion order.
    let hist = dir.join("history");
    std::fs::create_dir_all(&hist).unwrap();
    let base_ts = 1_700_000_000u64;
    for i in 0..5u64 {
        let ts = base_ts + i * 100;
        std::fs::write(
            hist.join(format!("list-{}.json", ts)),
            format!("{{\"name\":\"list\",\"system_prompt\":\"v{}\"}}", i),
        )
        .unwrap();
    }
    // A different agent's snapshot must not leak into the listing.
    std::fs::write(hist.join(format!("other-{}.json", base_ts + 500)), "{}").unwrap();

    let snaps = mgr.list_agent_history("list").unwrap();
    assert_eq!(snaps.len(), 5);
    let names: Vec<String> = snaps
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<String> = (0..5u64)
        .rev()
        .map(|i| format!("list-{}.json", base_ts + i * 100))
        .collect();
    assert_eq!(names, expected, "snapshots must be ordered newest-first");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `revert_agent` restores the snapshot's prompt and snapshots the CURRENT
/// file first, so a revert is itself reversible.
#[test]
fn test_revert_agent_restores_prompt_and_is_itself_reversible() {
    let dir = temp_agents_dir("revert");
    let mgr = AgentManager::new(dir.clone());
    mgr.add_agent(&hist_agent("rv", "v1")).unwrap();
    mgr.edit_agent("rv", &hist_agent("rv", "v2")).unwrap();
    mgr.edit_agent("rv", &hist_agent("rv", "v3")).unwrap();

    let snaps = mgr.list_agent_history("rv").unwrap();
    assert_eq!(snaps.len(), 2);
    // snaps[0] = newest (holds v2), snaps[1] = oldest (holds v1).
    let oldest = snaps[1].clone();

    let restored = mgr.revert_agent("rv", &oldest).unwrap();
    assert_eq!(restored.system_prompt, "v1");
    assert_eq!(restored.name, "rv");
    assert_eq!(mgr.get_agent("rv").unwrap().system_prompt, "v1");

    // The revert snapshotted the pre-revert (v3) state → 3 snapshots now.
    let snaps2 = mgr.list_agent_history("rv").unwrap();
    assert_eq!(snaps2.len(), 3);
    // Reverting to the newest snapshot goes FORWARD again to v3.
    let restored2 = mgr.revert_agent("rv", &snaps2[0]).unwrap();
    assert_eq!(restored2.system_prompt, "v3");
    assert_eq!(mgr.get_agent("rv").unwrap().system_prompt, "v3");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `revert_agent` rejects snapshots that are not a direct child of the
/// history dir, and snapshots belonging to a different agent.
#[test]
fn test_revert_agent_rejects_foreign_snapshots() {
    let dir = temp_agents_dir("reject");
    let mgr = AgentManager::new(dir.clone());
    mgr.add_agent(&hist_agent("aa", "A1")).unwrap();
    mgr.add_agent(&hist_agent("bb", "B1")).unwrap();
    mgr.edit_agent("aa", &hist_agent("aa", "A2")).unwrap();
    mgr.edit_agent("bb", &hist_agent("bb", "B2")).unwrap();

    let aa_snaps = mgr.list_agent_history("aa").unwrap();
    let bb_snaps = mgr.list_agent_history("bb").unwrap();
    assert!(!aa_snaps.is_empty() && !bb_snaps.is_empty());

    // A snapshot of agent "bb" cannot be reverted onto "aa".
    assert!(matches!(
        mgr.revert_agent("aa", &bb_snaps[0]),
        Err(crate::agents::AgentError::ConfigError(_))
    ));
    // "aa" is untouched.
    assert_eq!(mgr.get_agent("aa").unwrap().system_prompt, "A2");

    // A file outside the history dir (even in the agents dir) is rejected.
    let outside = dir.join("aa-outside.json");
    std::fs::write(&outside, "{}").unwrap();
    assert!(mgr.revert_agent("aa", &outside).is_err());
    // A nested path inside the history dir is also rejected (not a direct child).
    let nested = dir.join("history").join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    let nested_file = nested.join(format!("aa-{}.json", 1_700_000_000));
    std::fs::write(&nested_file, "{}").unwrap();
    assert!(mgr.revert_agent("aa", &nested_file).is_err());
    // And "aa" is still untouched.
    assert_eq!(mgr.get_agent("aa").unwrap().system_prompt, "A2");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Renaming an agent snapshots the OLD file under its OLD name before it is
/// deleted, so the pre-rename state stays recoverable from the listing.
#[test]
fn test_rename_snapshots_old_file() {
    let dir = temp_agents_dir("rename");
    let mgr = AgentManager::new(dir.clone());
    mgr.add_agent(&hist_agent("oldname", "old prompt")).unwrap();

    let mut renamed = hist_agent("newname", "new prompt");
    renamed.description = "renamed".to_string();
    mgr.edit_agent("oldname", &renamed).unwrap();

    assert!(!dir.join("oldname.json").exists());
    assert!(dir.join("newname.json").exists());
    assert_eq!(
        mgr.get_agent("newname").unwrap().system_prompt,
        "new prompt"
    );

    // The old file was snapshotted under its old name.
    let old_snaps = mgr.list_agent_history("oldname").unwrap();
    assert_eq!(old_snaps.len(), 1);
    let content = std::fs::read_to_string(&old_snaps[0]).unwrap();
    assert!(
        content.contains("old prompt"),
        "old snapshot content: {}",
        content
    );
    // No snapshot for the new name (it did not exist before the rename).
    assert!(mgr.list_agent_history("newname").unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
