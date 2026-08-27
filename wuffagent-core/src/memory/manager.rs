use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing;

use super::types::{MemoryConfig, MemoryEntry, MemoryType};
use super::search::search_memories;
use super::storage::{load_memories, save_memories, get_memories_path, count_active_memories};
use super::extractor::extract_memories;
use super::improver::suggest_improvements;
use crate::agents::llm_client::LlmClient;
use crate::types::Message;

/// Main orchestrator for the memory system.
pub struct MemoryManager {
    entries: Arc<Mutex<Vec<MemoryEntry>>>,
    config: MemoryConfig,
    storage_path: PathBuf,
    llm_client: Option<Arc<dyn LlmClient>>,
}

impl Clone for MemoryManager {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            config: self.config.clone(),
            storage_path: self.storage_path.clone(),
            llm_client: self.llm_client.clone(),
        }
    }
}

impl MemoryManager {
    /// Create a new MemoryManager and load existing memories.
    pub fn new(config: MemoryConfig) -> Result<Self, String> {
        let storage_path = get_memories_path(&config);
        let entries = match load_memories(&storage_path) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Failed to load memories from {:?}: {}", storage_path, e);
                Vec::new()
            }
        };
        Ok(Self {
            entries: Arc::new(Mutex::new(entries)),
            config,
            storage_path,
            llm_client: None,
        })
    }

    /// Create a new MemoryManager with an LLM client for extraction.
    pub fn new_with_llm(config: MemoryConfig, llm_client: Arc<dyn LlmClient>) -> Result<Self, String> {
        let mut m = Self::new(config)?;
        m.llm_client = Some(llm_client);
        Ok(m)
    }

    /// Get the current config.
    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    /// Extract memories from conversation messages and persist them.
    pub async fn extract_and_save(&self, messages: &[Message], source: &str) -> Result<usize, String> {
        let entries = extract_memories(self, messages, source, self.llm_client.as_ref().map(|c| c.as_ref())).await?;
        let count = entries.len();
        if count == 0 {
            return Ok(0);
        }
        self.add_batch(entries)?;
        Ok(count)
    }

    /// Suggest improvements for an agent based on memories and recent task.
    pub async fn suggest_improvements(
        &self,
        agent_config: &crate::agents::config::AgentConfig,
        task: &str,
        result: &str,
    ) -> Result<Vec<super::improver::ImprovementSuggestion>, String> {
        let llm = match &self.llm_client {
            Some(c) => c.clone(),
            None => return Ok(Vec::new()),
        };
        suggest_improvements(self, agent_config, task, result, llm.as_ref()).await
    }

    /// Search for relevant memories.
    /// Releases the mutex before running the search to avoid blocking other operations.
    pub fn search(&self, query: &str) -> Vec<MemoryEntry> {
        if !self.config.enabled {
            return Vec::new();
        }

        let entries = {
            let entries = self.entries.lock().unwrap();
            entries.clone()
        }; // mutex released before search
        let results = search_memories(&entries, query, &self.config);
        results.into_iter().cloned().collect()
    }

    /// Get recent memories (for fallback).
    /// Releases the mutex before copying to avoid blocking other operations.
    pub fn get_recent(&self, count: usize) -> Vec<MemoryEntry> {
        if !self.config.enabled {
            return Vec::new();
        }

        let entries = {
            let entries = self.entries.lock().unwrap();
            entries.iter()
                .rev()
                .take(count)
                .cloned()
                .collect::<Vec<_>>()
        }; // mutex released
        entries
    }

    /// Get all active (non-expired, non-superseded) memories.
    /// Releases the mutex before copying.
    pub fn get_all_memories(&self) -> Vec<MemoryEntry> {
        let entries = {
            let entries = self.entries.lock().unwrap();
            entries.iter()
                .filter(|e| !e.is_expired() && e.supersedes.is_none())
                .cloned()
                .collect::<Vec<_>>()
        }; // mutex released
        entries
    }

    /// Add a new memory entry.
    pub fn add(&self, entry: MemoryEntry) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        entries.push(entry);
        drop(entries);

        self.evict_if_needed()?;
        self.save()?;
        Ok(())
    }

    /// Add multiple memory entries.
    /// Uses fuzzy deduplication to avoid near-duplicate entries.
    pub fn add_batch(&self, entries: Vec<MemoryEntry>) -> Result<(), String> {
        let mut mem_entries = self.entries.lock().unwrap();
        for entry in entries {
            // Check for duplicates with fuzzy matching.
            let is_duplicate = mem_entries.iter().any(|existing| {
                // Exact match first
                if existing.content == entry.content
                    && existing.r#type == entry.r#type
                    && existing.tags == entry.tags
                {
                    return true;
                }
                // Fuzzy match: same type and high token overlap on content.
                if existing.r#type != entry.r#type {
                    return false;
                }
                let existing_tokens: std::collections::HashSet<&str> =
                    existing.content.split_whitespace().collect();
                let entry_tokens: std::collections::HashSet<&str> =
                    entry.content.split_whitespace().collect();
                let intersection = existing_tokens.intersection(&entry_tokens).count();
                let union = existing_tokens.union(&entry_tokens).count();
                union > 0 && (intersection as f64 / union as f64) > 0.75
            });
            if !is_duplicate {
                mem_entries.push(entry);
            }
        }
        drop(mem_entries);

        self.evict_if_needed()?;
        self.save()?;
        Ok(())
    }

    /// Update an existing memory entry by ID.
    pub fn update(&self, id: &str, content: &str) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.content = content.to_string();
            drop(entries);
            self.save()?;
            return Ok(());
        }
        Err(format!("Memory entry '{}' not found", id))
    }

    /// Delete a memory entry by ID.
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| e.id != id);
        drop(entries);
        self.save()?;
        Ok(())
    }

    /// Mark an entry as superseded (used during summarization).
    pub fn supersede(&self, id: &str, new_id: &str) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.supersedes = Some(new_id.to_string());
            drop(entries);
            self.save()?;
            return Ok(());
        }
        Err(format!("Memory entry '{}' not found", id))
    }

    /// Get the count of active memories.
    pub fn count(&self) -> usize {
        let entries = self.entries.lock().unwrap();
        count_active_memories(&entries)
    }

    /// Evict entries if we're approaching the max.
    fn evict_if_needed(&self) -> Result<(), String> {
        let entries = self.entries.lock().unwrap();
        let active_count = count_active_memories(&entries);

        if active_count <= self.config.max_entries {
            return Ok(());
        }

        // Sort by confidence (ascending) and timestamp (ascending) to evict oldest/lowest first
        let mut sorted: Vec<&MemoryEntry> = entries.iter()
            .filter(|e| !e.is_expired() && e.supersedes.is_none())
            .collect();
        sorted.sort_by(|a, b| {
            a.confidence.partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let ta = a.timestamp.unwrap_or_default();
                    let tb = b.timestamp.unwrap_or_default();
                    ta.cmp(&tb)
                })
        });

        // Collect IDs to remove BEFORE dropping the lock to avoid deadlock
        let to_remove = active_count - self.config.max_entries;
        let ids_to_remove: Vec<String> = sorted.iter()
            .take(to_remove)
            .map(|e| e.id.clone())
            .collect();
        drop(entries);

        // Remove entries under a fresh lock
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| !ids_to_remove.contains(&e.id));
        drop(entries);

        tracing::info!("Evicted {} memories, {} remaining", to_remove, self.count());
        self.save()
    }

    /// Save memories to disk.
    pub fn save(&self) -> Result<(), String> {
        let entries = self.entries.lock().unwrap();
        save_memories(&self.storage_path, &entries)
    }

    /// Build the memory context block for injection into system prompts.
    pub fn build_context_block(&self, query: &str) -> String {
        if !self.config.enabled || self.config.injection_mode == super::types::InjectionMode::Off {
            return String::new();
        }

        let memories = if query.trim().is_empty() {
            let recent = self.get_recent(self.config.injection_max_entries);
            recent
        } else {
            let results = self.search(query);
            if self.config.injection_mode == super::types::InjectionMode::Smart && results.is_empty() {
                return String::new();
            }
            results
        };

        if memories.is_empty() {
            return String::new();
        }

        let mut block = String::from("\n═══ MEMORY CONTEXT ═══\n(Relevant memories from past sessions)\n\n");
        let mut chars = 0;

        for memory in &memories {
            let line = format!("[{}] {}\n", memory.r#type, memory.content);
            if chars + line.len() > self.config.injection_max_chars {
                break;
            }
            block.push_str(&line);
            chars += line.len();
        }

        block.push_str("══════════════════════\n");
        block
    }

    /// Extract memory type from a string (for LLM parsing).
    pub fn parse_memory_type(s: &str) -> MemoryType {
        match s.to_lowercase().as_str() {
            "fact" => MemoryType::Fact,
            "lesson" => MemoryType::Lesson,
            "decision" => MemoryType::Decision,
            "context" => MemoryType::Context,
            "goal" => MemoryType::Goal,
            _ => MemoryType::Fact,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_add_and_search() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        manager.add(MemoryEntry::new(
            MemoryType::Fact,
            "WuffAgent uses Cargo workspace",
            "test",
            &["project"],
        )).unwrap();

        let results = manager.search("cargo");
        assert!(!results.is_empty());
    }

    #[test]
    fn test_eviction() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            max_entries: 3,
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        for i in 0..5 {
            manager.add(MemoryEntry::new(
                MemoryType::Fact,
                &format!("Memory {}", i),
                "test",
                &[],
            )).unwrap();
        }

        assert_eq!(manager.count(), 3);
    }

    #[test]
    fn test_build_context_block() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            injection_max_entries: 2,
            injection_max_chars: 200,
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        manager.add(MemoryEntry::new(
            MemoryType::Fact,
            "WuffAgent is a Rust project",
            "test",
            &["project"],
        )).unwrap();

        manager.add(MemoryEntry::new(
            MemoryType::Lesson,
            "Shell tool uses nested config",
            "test",
            &["shell"],
        )).unwrap();

        let block = manager.build_context_block("rust project");
        assert!(block.contains("WuffAgent is a Rust project"));
        assert!(block.contains("═══ MEMORY CONTEXT ═══"));
    }
}
