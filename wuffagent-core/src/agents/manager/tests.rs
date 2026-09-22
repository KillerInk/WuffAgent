//! Unit tests for the manager module (see super).

use super::*;

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
