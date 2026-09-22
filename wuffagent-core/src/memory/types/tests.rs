//! Unit tests for the `types` module (see `super`).

use super::*;

#[test]
fn test_legacy_llm_search_mode_migrates_to_keyword() {
    let json = r#"{"enabled": true, "search_mode": "llm"}"#;
    let config: MemoryConfig = serde_json::from_str(json).expect("legacy config should load");
    assert_eq!(config.search_mode, SearchMode::Keyword);
}

#[test]
fn test_keyword_search_mode_parses() {
    let json = r#"{"enabled": true, "search_mode": "keyword"}"#;
    let config: MemoryConfig = serde_json::from_str(json).expect("config should load");
    assert_eq!(config.search_mode, SearchMode::Keyword);
}

#[test]
fn test_unknown_search_mode_rejected() {
    let json = r#"{"enabled": true, "search_mode": "vector"}"#;
    let config: Result<MemoryConfig, _> = serde_json::from_str(json);
    assert!(config.is_err());
}

#[test]
fn test_missing_maintenance_fields_use_defaults() {
    // Old config files predate the batched-maintenance fields; they must
    // load with the new defaults.
    let json = r#"{"enabled": true}"#;
    let config: MemoryConfig = serde_json::from_str(json).expect("old config should load");
    assert_eq!(config.memory_maintenance_batch_size, 15);
    assert_eq!(config.memory_maintenance_timeout_secs, 600);
}

#[test]
fn test_legacy_auto_extract_field_ignored() {
    // T1 removed auto_extract_after_task; old configs must still load.
    let json = r#"{"enabled": true, "auto_extract_after_task": true}"#;
    let config: MemoryConfig = serde_json::from_str(json).expect("legacy config should load");
    assert!(config.enabled);
}

#[test]
fn test_i4_cooldown_and_auto_improve_defaults() {
    // I4 flipped auto_improve to ON — the cost control (cooldown + evidence
    // gate) makes the idle check cheap.
    let config = MemoryConfig::default();
    assert!(config.auto_improve, "I4: auto_improve defaults to true");
    assert_eq!(config.improvement_cooldown_tasks, 5);

    // Old config files predate the cooldown field; they must load with the
    // new default (schema churn is serde-defaulted, not breaking).
    let legacy: MemoryConfig =
        serde_json::from_str(r#"{"enabled": true}"#).expect("old config should load");
    assert_eq!(legacy.improvement_cooldown_tasks, 5);
    assert!(
        legacy.auto_improve,
        "I4: default flipped even for legacy files"
    );

    // An EXPLICIT "auto_improve": false in an existing config file is
    // respected (the default only fills in missing values).
    let explicit: MemoryConfig =
        serde_json::from_str(r#"{"auto_improve": false}"#).expect("config should load");
    assert!(!explicit.auto_improve);
}
