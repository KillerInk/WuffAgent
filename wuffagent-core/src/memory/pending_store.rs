//! G.1: persistence of pending self-improvement suggestions.
//!
//! The review panel used to hold unreviewed suggestions in memory only, so
//! closing WuffAgent dropped every suggestion the user had not reviewed yet.
//! This module stores them in `<wuffagent_home>/pending_improvements.json`
//! (a versioned JSON document wrapping the serde-able core
//! [`ImprovementSuggestion`] type) so they survive an exit; the panel loads
//! the queue back at startup and saves it on every change.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::types::ImprovementSuggestion;

/// Document envelope: versioned so the on-disk format can evolve (unknown
/// fields are ignored, missing ones default) without breaking old files.
#[derive(Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    items: Vec<ImprovementSuggestion>,
}

/// File-backed store of pending improvement suggestions.
#[derive(Clone)]
pub struct PendingStore {
    path: PathBuf,
}

impl PendingStore {
    /// Store at the canonical location
    /// (`<wuffagent_home>/pending_improvements.json`).
    pub fn new() -> Self {
        Self {
            path: crate::config::get_wuffagent_home().join("pending_improvements.json"),
        }
    }

    /// Store at an explicit path (tests, or a future per-project store).
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the pending suggestions. A missing file yields an empty list; a
    /// corrupt or unreadable file also yields an empty list (with a warning)
    /// and is left untouched for inspection — a damaged suggestions file
    /// should not be silently clobbered by the next save.
    pub fn load(&self) -> Vec<ImprovementSuggestion> {
        match fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str::<Doc>(&text) {
                Ok(doc) => doc.items,
                Err(err) => {
                    warn!(
                        path = %self.path.display(),
                        error = %err,
                        "pending_improvements.json failed to parse; treating as empty"
                    );
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                warn!(
                    path = %self.path.display(),
                    error = %err,
                    "could not read pending_improvements.json; treating as empty"
                );
                Vec::new()
            }
        }
    }

    /// Persist the pending list atomically (temp file + rename, the
    /// codebase convention). Saving an EMPTY list removes the file, so a
    /// fully reviewed queue does not leave an empty JSON doc behind in the
    /// home dir.
    pub fn save(&self, items: &[ImprovementSuggestion]) -> io::Result<()> {
        if items.is_empty() {
            return match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err),
            };
        }
        let doc = Doc {
            version: 1,
            items: items.to_vec(),
        };
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pending_improvements.json");
        let tmp = self.path.with_file_name(format!("{file_name}.tmp"));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)
    }
}

impl Default for PendingStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(agent: &str, prompt: Option<&str>) -> ImprovementSuggestion {
        ImprovementSuggestion {
            agent_name: agent.to_string(),
            prompt_change: prompt.map(str::to_string),
            rationale: format!("rationale for {agent}"),
            new_agents: vec![],
            description: None,
            allowed_tools: None,
            reasoning_effort: None,
            shell_config: None,
            handoff_targets: None,
            task_timeout_ms: None,
            skill_updates: vec![],
            evidence: vec!["lesson: something went wrong".to_string()],
        }
    }

    /// Unique temp dir per test (tests run in one process, so pid + a
    /// per-call counter is enough; no tempfile dependency in core).
    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "wuffagent-test-pending-{name}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn round_trip_preserves_items() {
        let dir = temp_dir("rt");
        let store = PendingStore::at(dir.join("pending_improvements.json"));
        let items = vec![
            suggestion("coder", Some("new prompt")),
            suggestion("researcher", None),
        ];
        store.save(&items).expect("save");
        let loaded = store.load();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].agent_name, "coder");
        assert_eq!(loaded[0].prompt_change.as_deref(), Some("new prompt"));
        assert_eq!(loaded[1].agent_name, "researcher");
        assert_eq!(loaded[1].prompt_change, None);
        assert_eq!(loaded[0].evidence, items[0].evidence);
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = temp_dir("missing");
        let store = PendingStore::at(dir.join("nope.json"));
        assert!(store.load().is_empty());
    }

    #[test]
    fn corrupt_file_is_empty_and_untouched() {
        let dir = temp_dir("corrupt");
        let path = dir.join("pending_improvements.json");
        fs::write(&path, "this is { not json").expect("write");
        let store = PendingStore::at(path.clone());
        assert!(store.load().is_empty());
        // Left in place for inspection (not clobbered by the failed read).
        assert_eq!(fs::read_to_string(&path).unwrap(), "this is { not json");
    }

    #[test]
    fn save_empty_removes_file() {
        let dir = temp_dir("empty");
        let store = PendingStore::at(dir.join("pending_improvements.json"));
        store.save(&[suggestion("coder", None)]).expect("save");
        assert!(store.path().exists());
        store.save(&[]).expect("save empty");
        assert!(!store.path().exists());
        // Saving empty again (file already gone) is still Ok.
        store.save(&[]).expect("save empty twice");
    }

    #[test]
    fn old_document_without_new_fields_still_parses() {
        // The pre-I2 schema had only agent_name/prompt_change/rationale/
        // new_agents — serde defaults must cover the rest.
        let dir = temp_dir("legacy");
        let path = dir.join("pending_improvements.json");
        fs::write(
            &path,
            r#"{"version":1,"items":[{"agent_name":"coder","prompt_change":"x","rationale":"r","new_agents":[]}]}"#,
        )
        .expect("write");
        let store = PendingStore::at(path);
        let loaded = store.load();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].agent_name, "coder");
        assert!(loaded[0].evidence.is_empty());
        assert!(loaded[0].skill_updates.is_empty());
    }

    #[test]
    fn no_tmp_file_left_behind() {
        let dir = temp_dir("tmp");
        let store = PendingStore::at(dir.join("pending_improvements.json"));
        store.save(&[suggestion("coder", Some("p"))]).expect("save");
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");
    }
}
