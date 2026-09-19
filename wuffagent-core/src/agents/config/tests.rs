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
    assert_eq!(config.shell_config.allowed_commands, vec!["cargo build.*".to_string(), "git.*".to_string()]);
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
    search_agent.save_to_file(&search_dir.join("search_agent.json")).unwrap();

    // Place an agent in the primary dir
    let primary_agent = AgentConfig {
        name: "primary_agent".to_string(),
        description: "From primary dir".to_string(),
        system_prompt: "Primary prompt".to_string(),
        allowed_tools: vec!["file_io".to_string()],
        enabled: true,
        ..Default::default()
    };
    primary_agent.save_to_file(&primary_dir.join("primary_agent.json")).unwrap();

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
    assert!(legacy.handoff_enabled, "legacy handoff_enabled must migrate");
    assert_eq!(legacy.handoff_targets, vec!["coder"], "legacy can_invoke must migrate");

    assert!(load_agent_from_dir(&dir, "off").is_none(), "disabled agent must not load");
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
    assert_eq!(helper.agents_search_dirs, vec![primary.clone(), search_b.clone()]);

    assert!(load_agent_from_dirs(&dirs, "missing").is_none());

    for d in [&primary, &search_a, &search_b] {
        let _ = std::fs::remove_dir_all(d);
    }
}
