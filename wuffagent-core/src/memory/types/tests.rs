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
