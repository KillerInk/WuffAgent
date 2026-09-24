//! Tests for `list_agents` / `edit_agent_profile` (T1).
//!
//! All tests use throwaway dirs under the system temp dir (same convention as
//! `agents/manager/tests.rs`); nothing outside is touched.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::agents::config::AgentConfig;
use crate::agents::manager::AgentManager;
use crate::tools::types::{Tool, ToolOutput, ToolParams};

/// Throwaway root dir for one test (removed first if it exists).
fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent_agent_profile_test_{}_{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn make_agent(name: &str, prompt: &str) -> AgentConfig {
    AgentConfig {
        name: name.into(),
        description: format!("{} description", name),
        system_prompt: prompt.into(),
        allowed_tools: vec!["read_file".into(), "shell".into()],
        enabled: true,
        task_timeout_ms: 60_000,
        shell_config: crate::types::ShellConfig::default(),
        agents_dir: PathBuf::new(),
        agents_search_dirs: Vec::new(),
        custom_prompts: std::collections::HashMap::new(),
        reasoning_effort: crate::types::ReasoningEffort::default(),
        trim_config: crate::trimming::config::TrimConfig::default(),
        handoff_enabled: false,
        handoff_targets: Vec::new(),
        restart_enabled: false,
    }
}

fn call(
    tool: &EditAgentProfileTool,
    json: serde_json::Value,
) -> crate::tools::types::ToolResult<ToolOutput> {
    let values = json
        .as_object()
        .expect("params must be an object")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    tool.execute(ToolParams { values })
}

fn success(v: ToolOutput) -> serde_json::Value {
    match v {
        ToolOutput::Success(v) => v,
        ToolOutput::Error(e) => panic!("expected success, got error: {}", e),
    }
}

fn read_profile(dir: &Path, name: &str) -> serde_json::Value {
    let content = std::fs::read_to_string(dir.join(format!("{}.json", name)))
        .unwrap_or_else(|e| panic!("profile file {} missing: {}", name, e));
    serde_json::from_str(&content).expect("profile parses")
}

fn history_snapshots(dir: &Path, name: &str) -> Vec<PathBuf> {
    let hist = dir.join("history");
    let Ok(entries) = std::fs::read_dir(&hist) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.to_string_lossy()
                .contains(&format!("{}-", name))
                && p.extension().map(|x| x == "json").unwrap_or(false)
        })
        .collect()
}

#[test]
fn test_valid_profile_name() {
    assert!(valid_profile_name("coder"));
    assert!(valid_profile_name("a-b_c9"));
    assert!(!valid_profile_name(""));
    assert!(!valid_profile_name("."));
    assert!(!valid_profile_name(".."));
    assert!(!valid_profile_name("a/b"));
    assert!(!valid_profile_name("a\\b"));
    assert!(!valid_profile_name("a:b"));
}

#[test]
fn test_list_agents_shape_and_dedup() {
    let root = temp_root("list");
    let primary = root.join("primary");
    let search = root.join("search");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&search).unwrap();

    let mut manager = AgentManager::new(primary.clone());
    manager.add_search_dir(search.clone());
    let mgr = Arc::new(manager);
    mgr.add_agent(&make_agent("coder", "You are a coder.")).unwrap();
    let mut disabled = make_agent("broken", "off");
    disabled.enabled = false;
    mgr.add_agent(&disabled).unwrap();

    // A profile in the SEARCH dir colliding with a primary name — primary wins.
    let mut shadow = make_agent("coder", "shadow prompt");
    shadow.description = "shadow".into();
    shadow.save_to_file(&search.join("coder.json")).unwrap();
    // A legacy WorkerConfig file in the search dir (migrated on read).
    std::fs::write(
        search.join("legacy.json"),
        r#"{"name":"legacy-bot","personality":"old style","can_invoke":["coder"],"handoff_enabled":true}"#,
    )
    .unwrap();

    let tool = ListAgentsTool::new(mgr);
    let v = success(tool.execute(ToolParams::default()).expect("list_agents runs"));
    let profiles = v["profiles"].as_array().expect("profiles array");
    let find = |name: &str| -> &serde_json::Value {
        profiles
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("profile '{}' missing in {:?}", name, profiles))
    };

    assert_eq!(profiles.len(), 3, "coder + broken + legacy-bot, no shadow dup: {:?}", profiles);
    // Primary wins the dedup.
    assert!(find("coder")["system_prompt_preview"]
        .as_str()
        .unwrap()
        .starts_with("You are a coder"));
    assert_eq!(
        find("coder")["path"].as_str().unwrap(),
        primary.join("coder.json").to_str().unwrap()
    );
    // Disabled profiles are listed (an editor must be able to re-enable them).
    assert_eq!(find("broken")["enabled"], false);
    // Legacy file parsed + migrated (can_invoke → handoff_targets).
    assert_eq!(find("legacy-bot")["handoff_targets"][0], "coder");
    assert_eq!(find("legacy-bot")["handoff_enabled"], true);
    // Preview + size bookkeeping.
    assert!(find("coder")["system_prompt_chars"].as_u64().unwrap() > 0);
    assert!(find("coder")["description"].as_str().is_some());
}

#[test]
fn test_edit_description_and_prompt() {
    let root = temp_root("edit");
    let primary = root.join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let mgr = Arc::new(AgentManager::new(primary.clone()));
    mgr.add_agent(&make_agent("coder", "You are a coder.")).unwrap();

    let tool = EditAgentProfileTool::new(mgr.clone());
    let v = success(
        call(
            &tool,
            serde_json::json!({
                "name": "coder",
                "description": "tuned",
                "system_prompt": "You are a senior coder.",
                "enabled": false
            }),
        )
        .expect("edit runs"),
    );
    assert_eq!(v["agent"], "coder");
    assert!(v["applied"].as_array().unwrap().iter().any(|x| x == "description"));

    let file = read_profile(&primary, "coder");
    assert_eq!(file["description"], "tuned");
    assert_eq!(file["system_prompt"], "You are a senior coder.");
    assert_eq!(file["enabled"], false);
    // Untouched fields survive.
    assert_eq!(file["task_timeout_ms"], 60_000);
    assert_eq!(file["allowed_tools"][0], "read_file");

    // F4: a pre-edit snapshot exists next to the profile.
    let snaps = history_snapshots(&primary, "coder");
    assert_eq!(snaps.len(), 1, "one snapshot after one edit: {:?}", snaps);
    let snap: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&snaps[0]).unwrap()).unwrap();
    assert_eq!(snap["system_prompt"], "You are a coder.");
}

#[test]
fn test_edit_writes_to_search_dir_in_place() {
    let root = temp_root("inplace");
    let primary = root.join("primary");
    let search = root.join("search");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&search).unwrap();

    let mut manager = AgentManager::new(primary.clone());
    manager.add_search_dir(search.clone());
    let mgr = Arc::new(manager);

    // Profile exists ONLY in the search dir.
    let mut proj = make_agent("proj-agent", "project prompt");
    proj.description = "project-level profile".into();
    proj.save_to_file(&search.join("proj-agent.json")).unwrap();

    let tool = EditAgentProfileTool::new(mgr);
    let v = success(
        call(
            &tool,
            serde_json::json!({"name": "proj-agent", "description": "updated in place"}),
        )
        .expect("edit runs"),
    );
    assert_eq!(
        v["path"].as_str().unwrap(),
        search.join("proj-agent.json").to_str().unwrap()
    );

    let file = read_profile(&search, "proj-agent");
    assert_eq!(file["description"], "updated in place");
    assert_eq!(file["system_prompt"], "project prompt");
    assert!(
        !primary.join("proj-agent.json").exists(),
        "no shadow copy in the primary dir"
    );
    // F4 snapshot next to the file (not in the primary dir).
    assert_eq!(history_snapshots(&search, "proj-agent").len(), 1);
}

#[test]
fn test_rename_and_collision() {
    let root = temp_root("rename");
    let primary = root.join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let mgr = Arc::new(AgentManager::new(primary.clone()));
    mgr.add_agent(&make_agent("coder", "You are a coder.")).unwrap();
    mgr.add_agent(&make_agent("other", "You are other.")).unwrap();

    let tool = EditAgentProfileTool::new(mgr.clone());
    let v = success(
        call(
            &tool,
            serde_json::json!({"name": "coder", "new_name": "senior-coder"}),
        )
        .expect("rename runs"),
    );
    assert_eq!(v["agent"], "senior-coder");
    assert!(!primary.join("coder.json").exists(), "old file removed");
    let file = read_profile(&primary, "senior-coder");
    assert_eq!(file["name"], "senior-coder");
    assert_eq!(file["system_prompt"], "You are a coder.");

    // Collision: renaming onto an existing name is rejected, files untouched.
    let err = call(
        &tool,
        serde_json::json!({"name": "senior-coder", "new_name": "other"}),
    )
    .expect_err("collision must fail");
    assert!(err.to_string().contains("already exists"));
    assert!(primary.join("senior-coder.json").exists());
    assert!(primary.join("other.json").exists());
}

#[test]
fn test_self_removal_guard() {
    let root = temp_root("selfremoval");
    let primary = root.join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let mgr = Arc::new(AgentManager::new(primary.clone()));

    let mut selfy = make_agent("selfy", "p");
    selfy.allowed_tools = vec![
        "read_file".into(),
        "list_agents".into(),
        "edit_agent_profile".into(),
    ];
    mgr.add_agent(&selfy).unwrap();

    let tool = EditAgentProfileTool::new(mgr.clone());
    // Removing the profile tools without the flag → refused.
    let err = call(
        &tool,
        serde_json::json!({"name": "selfy", "allowed_tools": ["read_file"]}),
    )
    .expect_err("self-removal must be refused");
    assert!(err.to_string().contains("allow_self_removal"));
    // With the flag → applied.
    success(
        call(
            &tool,
            serde_json::json!({
                "name": "selfy",
                "allowed_tools": ["read_file"],
                "allow_self_removal": true
            }),
        )
        .expect("flagged self-removal runs"),
    );
    assert_eq!(read_profile(&primary, "selfy")["allowed_tools"], serde_json::json!(["read_file"]));

    // A profile that never HAD the tools: setting a list without them is fine
    // without the flag (nothing is being removed).
    mgr.add_agent(&make_agent("plain", "p2")).unwrap();
    success(
        call(
            &tool,
            serde_json::json!({"name": "plain", "allowed_tools": ["web_search"]}),
        )
        .expect("removing absent tools needs no flag"),
    );
}

#[test]
fn test_validation_errors() {
    let root = temp_root("validation");
    let primary = root.join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let mgr = Arc::new(AgentManager::new(primary.clone()));
    mgr.add_agent(&make_agent("coder", "p")).unwrap();

    let tool = EditAgentProfileTool::new(mgr);
    let err = call(
        &tool,
        serde_json::json!({"name": "nope", "description": "x"}),
    )
    .expect_err("unknown name must fail");
    let msg = err.to_string();
    assert!(msg.contains("No agent profile named 'nope'"), "{}", msg);
    assert!(msg.contains("coder"), "available list in error: {}", msg);

    let err = call(
        &tool,
        serde_json::json!({"name": "a/b", "description": "x"}),
    )
    .expect_err("bad name must fail");
    assert!(err.to_string().contains("Invalid profile name"));

    let err = call(&tool, serde_json::json!({"name": "coder"})).expect_err("empty edit");
    assert!(err.to_string().contains("Nothing to change"));
}

#[test]
fn test_edit_shell_config() {
    let root = temp_root("shell");
    let primary = root.join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let mgr = Arc::new(AgentManager::new(primary.clone()));
    mgr.add_agent(&make_agent("coder", "p")).unwrap();

    let tool = EditAgentProfileTool::new(mgr);
    success(
        call(
            &tool,
            serde_json::json!({
                "name": "coder",
                "shell": {"shell_enabled": false, "allowed_commands": ["cargo build"]}
            }),
        )
        .expect("shell edit runs"),
    );
    let file = read_profile(&primary, "coder");
    assert_eq!(file["shell_config"]["shell_enabled"], false);
    assert_eq!(file["shell_config"]["allowed_commands"][0], "cargo build");
    // Untouched shell fields keep their previous values.
    assert_eq!(file["shell_config"]["shell_type"], "powershell");
}

#[test]
fn test_canonicalize_oddly_named_file() {
    // The repo's own `agents/general.json` shape: file name ≠ profile name.
    let root = temp_root("canonical");
    let primary = root.join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    let mgr = Arc::new(AgentManager::new(primary.clone()));

    let mut cfg = make_agent("generalist", "versatile");
    cfg.description = "General purpose worker".into();
    cfg.save_to_file(&primary.join("general.json")).unwrap();

    let tool = EditAgentProfileTool::new(mgr);
    let v = success(
        call(
            &tool,
            serde_json::json!({"name": "generalist", "description": "tuned"}),
        )
        .expect("edit of oddly-named profile runs"),
    );
    assert_eq!(v["path"].as_str().unwrap(), primary.join("generalist.json").to_str().unwrap());
    assert!(!primary.join("general.json").exists(), "oddly-named file renamed");
    let file = read_profile(&primary, "generalist");
    assert_eq!(file["name"], "generalist");
    assert_eq!(file["description"], "tuned");

    // The pre-rename state is restorable from the F4 snapshots (canonicalize
    // and edit_agent each take one; same second → seq suffix). Every snapshot
    // must hold the PRE-edit content.
    let snaps = history_snapshots(&primary, "generalist");
    assert!(!snaps.is_empty(), "snapshot(s) exist: {:?}", snaps);
    for s in &snaps {
        let snap: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(s).unwrap()).unwrap();
        assert_eq!(snap["name"], "generalist");
        assert_eq!(snap["description"], "General purpose worker");
    }
}

#[test]
fn test_find_agent_file_first_dir_wins_and_disabled() {
    let root = temp_root("find");
    let primary = root.join("primary");
    let search = root.join("search");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&search).unwrap();

    let prim = make_agent("dup", "primary version");
    prim.save_to_file(&primary.join("dup.json")).unwrap();
    let mut shadow = make_agent("dup", "search version");
    shadow.description = "shadow".into();
    shadow.save_to_file(&search.join("dup.json")).unwrap();
    let mut off = make_agent("off", "disabled one");
    off.enabled = false;
    off.save_to_file(&search.join("off.json")).unwrap();

    let dirs = vec![primary.clone(), search.clone()];
    let (dir, path, cfg) = find_agent_file(&dirs, "dup").expect("dup found");
    assert_eq!(dir, primary, "first dir wins");
    assert_eq!(path, primary.join("dup.json"));
    assert_eq!(cfg.system_prompt, "primary version");

    // Disabled profiles are found too (unlike load_agent_from_dirs).
    let (_, _, off_cfg) = find_agent_file(&dirs, "off").expect("disabled found");
    assert!(!off_cfg.enabled);

    assert!(find_agent_file(&dirs, "ghost").is_none());
}
