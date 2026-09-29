//! Unit tests for the improvement module (see super), split by area:
//! - serialization.rs: suggestion / new-agent-proposal serde round-trips
//! - collect.rs: collect_lessons search/tag/recent behavior
//! - suggest.rs: suggest_improvements prompt, trajectory, wider fields (I1-I3)
//! - cost.rs: evidence gate + state persistence (I4)
//! - effect.rs: effect check with approved-change markers (I5)
//! - skills.rs: skill retire-signal line (3b)
//! - fleet.rs: fleet-wide evidence + the fleet improver (2b(b)/2d)
//!
//! NOTE: skills.rs's e2e test flips the process-global skills-dir override
//! (serialized on its own lock, matching memory/skills/tests.rs).

use super::*;
use crate::memory::{MemoryConfig, MemoryEntry, MemoryType};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

mod chat;
mod collect;
mod cost;
mod effect;
mod fleet;
mod serialization;
mod skills;
mod suggest;

fn fresh_manager() -> (MemoryManager, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    (MemoryManager::new(config).unwrap(), dir)
}

/// Test double: captures the prompt it is given, returns a fixed response.
struct CaptureLlm {
    response: String,
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl LlmClient for CaptureLlm {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.prompts.lock().unwrap().push(
            messages
                .first()
                .map(|m| m.content.clone())
                .unwrap_or_default(),
        );
        Ok(self.response.clone())
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let text = self.complete(messages).await?;
        chunk_handler(text.clone());
        Ok(text)
    }
}

/// Manager with auto_improve on (trigger lowered to 1 lesson).
fn auto_improve_manager(dir: &std::path::Path) -> (MemoryManager, Arc<Mutex<Vec<String>>>, String) {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let config = MemoryConfig {
        enabled: true,
        auto_improve: true,
        improvement_trigger_lessons: 1,
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    (manager, prompts, dir.to_str().unwrap().to_string())
}

fn test_agent_config() -> crate::agents::config::AgentConfig {
    let mut cfg = crate::agents::config::AgentConfig::default();
    cfg.name = "coder".to_string();
    cfg.description = "writes code".to_string();
    cfg.system_prompt = "You are a coding agent. Be precise.".to_string();
    cfg
}


/// Backdate an entry's timestamp (MemoryEntry::new always stamps `Utc::now()`).
fn backdate(entry: &mut MemoryEntry, days: i64) {
    entry.timestamp = Some(chrono::Utc::now() - chrono::Duration::days(days));
}

/// M1: points `MetricsLog::default()` (read by `suggest_improvements`) at a
/// temp dir for the scope of a test. The override is PROCESS-GLOBAL, so the
/// guard holds a process-wide lock for the whole test — file-metrics tests
/// must serialize (same pattern as the MCP config-path tests).
static METRICS_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) struct MetricsDirGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    /// Kept alive so the temp dir outlives the test (the value itself is
    /// never read — only its cleanup on drop matters).
    _dir: tempfile::TempDir,
}

impl MetricsDirGuard {
    pub(crate) fn new() -> Self {
        let lock = METRICS_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        crate::agents::metrics::set_metrics_dir_for_testing(Some(dir.path().to_path_buf()));
        MetricsDirGuard {
            _lock: lock,
            _dir: dir,
        }
    }

    /// The temp dir the override currently points at (tests write known
    /// metric lines into it via `MetricsLog::new(guard.dir())`).
    pub(crate) fn dir(&self) -> &std::path::Path {
        self._dir.path()
    }
}

impl Drop for MetricsDirGuard {
    fn drop(&mut self) {
        crate::agents::metrics::set_metrics_dir_for_testing(None);
    }
}
