/// Configuration for intelligent context trimming.
///
/// Controls thresholds and behavior for the trimming pipeline that
/// summarizes tool results, build logs, and other verbose outputs
/// to reduce context bloat.

use serde::{Deserialize, Serialize};

/// Configuration for intelligent context trimming.
///
/// Tunable thresholds that control when and how aggressively tool
/// results and other content are summarized/truncated.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrimConfig {
    /// Whether trimming is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Hard cap on any tool result stored in messages.
    /// Results exceeding this are truncated regardless of type.
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,

    /// Maximum number of agent chain entries retained on session save.
    #[serde(default = "default_max_chain_entries")]
    pub max_chain_entries: usize,

    /// Maximum lines to keep for code blocks.
    #[serde(default = "default_code_max_lines")]
    pub code_max_lines: usize,

    /// Maximum lines to keep for build logs.
    #[serde(default = "default_log_max_lines")]
    pub log_max_lines: usize,

    /// Maximum items to keep for glob/file lists.
    #[serde(default = "default_list_max_items")]
    pub list_max_items: usize,

    /// Enable freshness-aware invalidation of stale/superseded `read_file`
    /// results at trim time (see `filestate`). Off = legacy size/age-only trim.
    #[serde(default = "default_true")]
    pub stale_file_invalidation: bool,
}

fn default_enabled() -> bool { true }
fn default_max_tool_result_chars() -> usize { 1000 }
fn default_max_chain_entries() -> usize { 50 }
fn default_code_max_lines() -> usize { 30 }
fn default_log_max_lines() -> usize { 15 }
fn default_list_max_items() -> usize { 20 }
fn default_true() -> bool { true }

impl Default for TrimConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tool_result_chars: default_max_tool_result_chars(),
            max_chain_entries: default_max_chain_entries(),
            code_max_lines: default_code_max_lines(),
            log_max_lines: default_log_max_lines(),
            list_max_items: default_list_max_items(),
            stale_file_invalidation: default_true(),
        }
    }
}

impl TrimConfig {
    /// Returns true if trimming is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
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
}
