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
}

#[test]
fn test_disabled_config() {
    let mut config = TrimConfig::default();
    config.enabled = false;
    assert!(!config.is_enabled());
}
