//! Unit tests for the improvement module (see super), split by area:
//! - serialization.rs: suggestion / new-agent-proposal serde round-trips
//! - collect.rs: collect_lessons search/tag/recent behavior
//! - suggest.rs: suggest_improvements prompt, trajectory, wider fields (I1-I3)
//! - cost.rs: evidence gate + state persistence (I4)
//! - effect.rs: effect check with approved-change markers (I5)

use super::*;
use crate::memory::{MemoryConfig, MemoryEntry, MemoryType};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

mod collect;
mod cost;
mod effect;
mod serialization;
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
