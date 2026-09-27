//! Unit tests for the `config` module (see `super`).

use super::*;

#[test]
fn test_default_config() {
    let config = TrimConfig::default();
    assert!(config.enabled);
    assert_eq!(config.max_tool_result_chars, 1000);
    assert_eq!(config.max_chain_entries, 50);
    assert_eq!(config.code_max_lines, 30);
    assert_eq!(config.log_max_lines, 15);
    assert_eq!(config.list_max_items, 20);
    assert!(config.stale_file_invalidation);
    assert_eq!(config.trim_trigger_pct, 90, "trigger default = proven 90%");
    assert_eq!(config.trim_target_pct, 50, "target default = proven 50%");
}

#[test]
fn test_trim_pcts_serde_missing_fields_default() {
    // Old config files predate the pcts fields: they must deserialize with
    // the proven 90/50 defaults (no behavior change on upgrade).
    let json = r#"{"enabled":true}"#;
    let config: TrimConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.trim_trigger_pct, 90);
    assert_eq!(config.trim_target_pct, 50);
    // Explicit values round-trip.
    let mut config = TrimConfig::default();
    config.trim_trigger_pct = 65;
    config.trim_target_pct = 30;
    let parsed: TrimConfig = serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
    assert_eq!(parsed, config);
}

#[test]
fn test_disabled_config() {
    let mut config = TrimConfig::default();
    config.enabled = false;
    assert!(!config.is_enabled());
}
