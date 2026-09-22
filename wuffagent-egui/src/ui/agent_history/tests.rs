//! Unit tests for the `agent_history` module (see `super`).

use super::*;
use wuffagent_core::agents::config::AgentConfig;

fn temp_agents_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent-egui-hist-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn agent(name: &str, prompt: &str) -> AgentConfig {
    AgentConfig {
        name: name.to_string(),
        description: "test".to_string(),
        system_prompt: prompt.to_string(),
        ..Default::default()
    }
}

#[test]
fn test_parse_ts_seq() {
    assert_eq!(parse_ts_seq("1234"), (1234, 0));
    assert_eq!(parse_ts_seq("1234-7"), (1234, 7));
    assert_eq!(parse_ts_seq("junk"), (0, 0));
    assert_eq!(parse_ts_seq(""), (0, 0));
    assert_eq!(parse_ts_seq("12-abc"), (12, 0));
}

#[test]
fn test_format_ts() {
    assert_eq!(format_ts(0), "1970-01-01 00:00 UTC");
    // 2020-01-01 00:00:00 UTC
    assert_eq!(format_ts(1577836800), "2020-01-01 00:00 UTC");
}

#[test]
fn test_list_history_spans_dirs_newest_first() {
    let dir_a = temp_agents_dir("a");
    let dir_b = temp_agents_dir("b");
    let dirs = vec![dir_a.clone(), dir_b.clone()];

    // "cap" lives in dir_b: two edits create two snapshots there.
    let mgr_b = AgentManager::new(dir_b.clone());
    mgr_b.add_agent(&agent("cap", "v0")).unwrap();
    mgr_b.edit_agent("cap", &agent("cap", "v1")).unwrap();
    mgr_b.edit_agent("cap", &agent("cap", "v2")).unwrap();

    // "other" lives in dir_a: one edit, one snapshot.
    let mgr_a = AgentManager::new(dir_a.clone());
    mgr_a.add_agent(&agent("other", "o0")).unwrap();
    mgr_a.edit_agent("other", &agent("other", "o1")).unwrap();

    // Merged across dirs, newest first, with correct dir attribution.
    let cap = list_history(&dirs, "cap");
    assert_eq!(cap.len(), 2, "expected 2 snapshots of 'cap'");
    assert!(cap[0].ts >= cap[1].ts, "newest first");
    assert!(cap.iter().all(|e| e.dir == dir_b), "snapshots belong to dir_b");
    assert!(cap.iter().all(|e| e.path.starts_with(dir_b.join("history"))));

    let other = list_history(&dirs, "other");
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].dir, dir_a);

    // Unknown agent -> empty, not an error.
    assert!(list_history(&dirs, "ghost").is_empty());

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn test_revert_restores_prompt_and_stays_reversible() {
    let dir = temp_agents_dir("revert");
    let mgr = AgentManager::new(dir.clone());
    mgr.add_agent(&agent("cap", "v0")).unwrap();
    mgr.edit_agent("cap", &agent("cap", "v1")).unwrap();
    mgr.edit_agent("cap", &agent("cap", "v2")).unwrap();

    let entries = list_history(&[dir.clone()], "cap");
    assert_eq!(entries.len(), 2);
    let oldest = &entries[entries.len() - 1];

    let restored = revert(&oldest.dir, "cap", oldest).expect("revert");
    assert_eq!(restored.system_prompt, "v0");

    // The file on disk is back to v0, and the revert itself snapshotted the
    // pre-revert (v2) state, so the history grew by one.
    assert_eq!(mgr.get_agent("cap").expect("cap").system_prompt, "v0");
    assert_eq!(list_history(&[dir.clone()], "cap").len(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_prompt_preview_truncates_and_flattens() {
    let dir = temp_agents_dir("preview");
    let mgr = AgentManager::new(dir.clone());
    let long = format!("You are a very specific agent. {}", "lorem ipsum dolor sit amet ".repeat(10));
    mgr.add_agent(&agent("cap", &long)).unwrap();
    mgr.edit_agent("cap", &agent("cap", "v2")).unwrap();

    let entries = list_history(&[dir.clone()], "cap");
    assert_eq!(entries.len(), 1);
    let preview = prompt_preview(&entries[0].path);
    assert!(!preview.contains('\n'), "preview must be single-line");
    assert!(preview.ends_with("..."), "long preview must be truncated: {}", preview);
    assert!(preview.starts_with("You are a very specific agent."));
    assert_eq!(preview.chars().count(), 60);

    // Unreadable file.
    assert_eq!(prompt_preview(&dir.join("nope.json")), "(unreadable)");

    let _ = std::fs::remove_dir_all(&dir);
}
