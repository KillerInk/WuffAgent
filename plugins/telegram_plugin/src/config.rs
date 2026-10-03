//! Config (`telegram.json`) + per-chat state (`telegram-state.json`).
//!
//! Both live in WuffAgent's config dir (`~/.wuffagent/`). The state file is
//! written atomically (temp file + rename) so a crash mid-write never leaves
//! a torn JSON file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const CONFIG_FILE: &str = "telegram.json";
pub const STATE_FILE: &str = "telegram-state.json";
pub const MAX_CHUNK_CHARS: usize = 4096;

fn default_session_name() -> String {
    "Telegram".to_string()
}
fn default_chunk_chars() -> usize {
    MAX_CHUNK_CHARS
}
fn default_poll_timeout() -> u64 {
    50
}

/// `~/.wuffagent/telegram.json`:
///
/// ```json
/// {
///   "token": "123456:AAH...",
///   "allow_chat_ids": [42],
///   "default_session": "Telegram",
///   "chunk_chars": 4096,
///   "api_base": null,
///   "poll_timeout_secs": 50
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// BotFather token.
    pub token: String,
    /// Chat ids the bot answers (empty = answers nobody).
    pub allow_chat_ids: Vec<i64>,
    /// Name for the auto-created default session of a chat.
    #[serde(default = "default_session_name")]
    pub default_session: String,
    /// Telegram hard limit is 4096; the plugin chunks longer replies.
    #[serde(default = "default_chunk_chars")]
    pub chunk_chars: usize,
    /// Override the API base URL (tests point this at a local fake server).
    #[serde(default)]
    pub api_base: Option<String>,
    /// getUpdates long-poll hold (seconds; 50 = Telegram's max).
    #[serde(default = "default_poll_timeout")]
    pub poll_timeout_secs: u64,
}

impl Config {
    pub fn load(dir: &Path) -> Result<Config, String> {
        let path = dir.join(CONFIG_FILE);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let cfg: Config =
            serde_json::from_str(&raw).map_err(|e| format!("{}: invalid JSON: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.token.trim().is_empty() {
            return Err("token is empty".into());
        }
        if !self.token.contains(':') {
            return Err(format!(
                "token {:?} looks malformed (expected '<id>:<secret>')",
                &self.token
            ));
        }
        if self.allow_chat_ids.is_empty() {
            return Err("allow_chat_ids is empty (the bot would answer nobody)".into());
        }
        if self.chunk_chars < 100 || self.chunk_chars > MAX_CHUNK_CHARS {
            return Err(format!(
                "chunk_chars {} out of range (100..=4096)",
                self.chunk_chars
            ));
        }
        Ok(())
    }
}

/// One chat's pointer into the session store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatState {
    pub session_id: String,
    pub session_name: String,
}

/// `~/.wuffagent/telegram-state.json`: chat_id -> session pointer, plus the
/// last seen Telegram `update_id` (offset) so a restart does not re-deliver
/// 24 h of old updates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BotStateFile {
    #[serde(default)]
    pub chats: BTreeMap<String, ChatState>,
    #[serde(default)]
    pub last_update_id: i64,
}

impl BotStateFile {
    /// Missing file -> default (first run). Corrupt file -> default + the
    /// corrupt file is moved aside (`.corrupt`) so it can be inspected but
    /// does not wedge the bot.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(STATE_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(file) => file,
                Err(e) => {
                    eprintln!("telegram_plugin: {} invalid ({e}); starting fresh", path.display());
                    let _ = std::fs::rename(&path, dir.join(format!("{}.corrupt", STATE_FILE)));
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Atomic write: serialize to a temp file in the same dir, then rename.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let path = dir.join(STATE_FILE);
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| format!("state serialize: {e}"))?;
        std::fs::write(&tmp, raw).map_err(|e| format!("state write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("state rename: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("tg_plugin_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn valid_cfg() -> Config {
        Config {
            token: "123456:TESTTOKEN".into(),
            allow_chat_ids: vec![42],
            default_session: "Telegram".into(),
            chunk_chars: 4096,
            api_base: None,
            poll_timeout_secs: 50,
        }
    }

    #[test]
    fn test_config_validate_rejects_bad_input() {
        let mut c = valid_cfg();
        c.token = "   ".into();
        assert!(c.validate().is_err(), "empty token");

        let mut c = valid_cfg();
        c.token = "no-colon-here".into();
        assert!(c.validate().is_err(), "token without ':'");

        let mut c = valid_cfg();
        c.allow_chat_ids.clear();
        assert!(c.validate().is_err(), "empty allowlist");

        let mut c = valid_cfg();
        c.chunk_chars = 99;
        assert!(c.validate().is_err(), "chunk_chars too small");
        c.chunk_chars = 4097;
        assert!(c.validate().is_err(), "chunk_chars too big");

        assert!(valid_cfg().validate().is_ok(), "valid config passes");
    }

    #[test]
    fn test_config_load_missing_and_invalid() {
        let dir = temp_dir("load");
        assert!(Config::load(&dir).is_err(), "missing file is an error");

        let mut c = valid_cfg();
        c.token = "x".into();
        std::fs::write(dir.join(CONFIG_FILE), serde_json::to_string(&c).unwrap()).unwrap();
        let err = Config::load(&dir).unwrap_err();
        assert!(err.contains("malformed"), "got: {err}");

        std::fs::write(dir.join(CONFIG_FILE), "{not json").unwrap();
        assert!(Config::load(&dir).unwrap_err().contains("invalid JSON"));

        std::fs::write(dir.join(CONFIG_FILE), serde_json::to_string(&valid_cfg()).unwrap()).unwrap();
        assert!(Config::load(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_state_save_load_roundtrip_and_corrupt_recovery() {
        let dir = temp_dir("state");

        let missing = BotStateFile::load(&dir);
        assert_eq!(missing, BotStateFile::default(), "missing file -> default");

        let mut file = BotStateFile::default();
        file.chats.insert(
            "42".into(),
            ChatState {
                session_id: "abc".into(),
                session_name: "Telegram".into(),
            },
        );
        file.last_update_id = 1234;
        file.save(&dir).unwrap();

        let loaded = BotStateFile::load(&dir);
        assert_eq!(loaded, file, "round trip");

        std::fs::write(dir.join(STATE_FILE), "{{{").unwrap();
        let recovered = BotStateFile::load(&dir);
        assert_eq!(recovered, BotStateFile::default(), "corrupt file -> default");
        assert!(dir.join(format!("{STATE_FILE}.corrupt")).exists(), "corrupt file moved aside");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
