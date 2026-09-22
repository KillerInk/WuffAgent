use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing;

use crate::agents::improvement::suggest_improvements;
use super::search::get_recent_memories;
use super::search::search_memories;
use super::storage::{count_active_memories, get_memories_path, load_memories, save_memories};
use super::types::{MemoryConfig, MemoryEntry, MemoryType};
use crate::llm::LlmClient;

/// Main orchestrator for the memory system.
///
/// `config` is interior-mutable so the UI (which holds an `Arc<MemoryManager>`)
/// can update memory settings at runtime without a `&mut` borrow.
pub struct MemoryManager {
    entries: Arc<Mutex<Vec<MemoryEntry>>>,
    config: Mutex<MemoryConfig>,
    storage_path: PathBuf,
    /// pub(crate): the maintenance impl block lives in the sibling
    /// `maintenance` module and checks/uses the client directly.
    pub(crate) llm_client: Option<Arc<dyn LlmClient>>,
}

impl Clone for MemoryManager {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            config: Mutex::new(self.config()),
            storage_path: self.storage_path.clone(),
            llm_client: self.llm_client.clone(),
        }
    }
}

impl MemoryManager {
    /// Create a new MemoryManager and load existing memories.
    pub fn new(config: MemoryConfig) -> Result<Self, String> {
        let storage_path = get_memories_path(&config);
        let mut entries = match load_memories(&storage_path) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Failed to load memories from {:?}: {}", storage_path, e);
                Vec::new()
            }
        };
        // Clean up expired and low-quality memories on load
        let removed = clean_stale_entries(&mut entries);
        if removed > 0 {
            tracing::info!("Cleaned {} stale memories on load", removed);
        }
        Ok(Self {
            entries: Arc::new(Mutex::new(entries)),
            config: Mutex::new(config),
            storage_path,
            llm_client: None,
        })
    }

    /// Create a new MemoryManager with an LLM client (used by the maintenance pass).
    pub fn new_with_llm(
        config: MemoryConfig,
        llm_client: Arc<dyn LlmClient>,
    ) -> Result<Self, String> {
        let mut m = Self::new(config)?;
        m.llm_client = Some(llm_client);
        Ok(m)
    }

    /// Get a clone of the current config.
    pub fn config(&self) -> MemoryConfig {
        self.config.lock().unwrap().clone()
    }

    /// Replace the config at runtime (used by the settings UI).
    pub fn set_config(&self, config: MemoryConfig) {
        *self.config.lock().unwrap() = config;
    }

    /// Suggest improvements for an agent based on memories and recent task.
    pub async fn suggest_improvements(
        &self,
        agent_config: &crate::agents::config::AgentConfig,
        task: &str,
        result: &str,
        stats: &crate::agents::RunStats,
    ) -> Result<Vec<crate::types::ImprovementSuggestion>, String> {
        let llm = match &self.llm_client {
            Some(c) => c.clone(),
            None => return Ok(Vec::new()),
        };
        suggest_improvements(self, agent_config, task, result, stats, llm.as_ref()).await
    }

    /// Search for relevant memories.
    /// Releases the mutex before running the search to avoid blocking other operations.
    pub fn search(&self, query: &str) -> Vec<MemoryEntry> {
        let config = self.config();
        if !config.enabled {
            return Vec::new();
        }

        let entries = self.entries.lock().unwrap();
        search_memories(&*entries, query, &config)
            .into_iter()
            .map(|e| (*e).clone())
            .collect()
    }

    /// Get all active (non-expired, non-superseded) memories carrying the
    /// exact `tag`. Case-insensitive on the tag value; order is store order.
    /// Releases the mutex before copying.
    pub fn get_by_tag(&self, tag: &str) -> Vec<MemoryEntry> {
        if !self.config().enabled {
            return Vec::new();
        }

        let tag_lower = tag.to_lowercase();
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !e.is_expired() && e.supersedes.is_none())
            .filter(|e| e.tags.iter().any(|t| t.eq_ignore_ascii_case(&tag_lower)))
            .cloned()
            .collect()
    }

    /// Get recent memories (for fallback).
    /// Skips expired and superseded entries; newest first.
    /// Releases the mutex before copying to avoid blocking other operations.
    pub fn get_recent(&self, count: usize) -> Vec<MemoryEntry> {
        if !self.config().enabled {
            return Vec::new();
        }

        let entries = {
            let guard = self.entries.lock().unwrap();
            get_recent_memories(&guard, count)
                .into_iter()
                .cloned()
                .collect()
        }; // mutex released
        entries
    }

    /// Get all active (non-expired, non-superseded) memories.
    /// Releases the mutex before copying.
    pub fn get_all_memories(&self) -> Vec<MemoryEntry> {
        let entries = {
            let entries = self.entries.lock().unwrap();
            entries
                .iter()
                .filter(|e| !e.is_expired() && e.supersedes.is_none())
                .cloned()
                .collect::<Vec<_>>()
        }; // mutex released
        entries
    }

    /// I4 (cost control): whether new improvement evidence has arrived since
    /// the last improver check.
    ///
    /// Evidence is any **Lesson-type** entry — that covers every kind the
    /// improver learns from (S1 verification outcomes, S2 user feedback,
    /// S3 agent lessons, F5 rejection lessons). Counted across all agents.
    ///
    /// When no check has been recorded yet (missing or corrupt state file),
    /// evidence exists if ANY lesson exists at all — the first check then
    /// records the baseline.
    pub fn has_new_improvement_evidence(&self) -> bool {
        let last_check = load_improvement_state(&self.improvement_state_path())
            .last_check
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));
        let memories = self.get_all_memories();
        match last_check {
            // Some Lesson entry strictly newer than the last check.
            Some(ts) => memories.iter().any(|e| {
                e.r#type == MemoryType::Lesson && e.timestamp.map(|t| t > ts).unwrap_or(false)
            }),
            None => memories.iter().any(|e| e.r#type == MemoryType::Lesson),
        }
    }

    /// I4 (cost control): record that an improvement check just ran, so the
    /// evidence gate stays closed until new Lesson entries arrive.
    ///
    /// Best-effort: any failure is only logged — the state file must never
    /// be able to break task completion.
    pub fn record_improvement_check(&self) {
        let path = self.improvement_state_path();
        let now = chrono::Utc::now();
        let state = ImprovementState {
            last_check: Some(now.timestamp()),
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::debug!("[MEMORY] Could not create state dir {:?}: {}", parent, e);
                return;
            }
        }
        let content = match serde_json::to_string(&state) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("[MEMORY] Could not serialize improvement state: {}", e);
                return;
            }
        };
        // Atomic write (temp + rename), mirroring `save_memories`.
        let temp_path = path.with_extension("json.tmp");
        if let Err(e) =
            std::fs::write(&temp_path, content).and_then(|_| std::fs::rename(&temp_path, &path))
        {
            let _ = std::fs::remove_file(&temp_path);
            tracing::debug!(
                "[MEMORY] Could not write improvement state {:?}: {}",
                path,
                e
            );
        }
    }

    /// I4: the improvement-check state file, a sibling of the project memory
    /// file (e.g. `improvement_state.json` next to `default.json`).
    fn improvement_state_path(&self) -> PathBuf {
        self.storage_path
            .parent()
            .map(|p| p.join("improvement_state.json"))
            .unwrap_or_else(|| PathBuf::from("improvement_state.json"))
    }

    /// Add a new memory entry.
    ///
    /// Runs a deduplication gate first: if a strong near-duplicate already
    /// exists, the new entry is NOT inserted and the existing entry is
    /// returned to the caller so it can react (e.g. suggest consolidation).
    pub fn add(&self, entry: MemoryEntry) -> Result<MemoryAddResult, String> {
        let mut guard = self.entries.lock().unwrap();
        let existing = guard.iter().find(|e| is_near_duplicate(e, &entry)).cloned();
        if let Some(existing) = existing {
            return Ok(MemoryAddResult::Duplicate(existing));
        }
        let created = entry.clone();
        guard.push(entry);
        drop(guard);

        self.evict_if_needed()?;
        self.save()?;
        Ok(MemoryAddResult::Created(created))
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
    ///
    /// Updating an entry revives it (clears `supersedes`) and, when provided,
    /// replaces its tags. Returns the updated entry.
    pub fn update(
        &self,
        id: &str,
        content: &str,
        tags: Option<Vec<String>>,
    ) -> Result<MemoryEntry, String> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.content = content.to_string();
            entry.supersedes = None;
            if let Some(tags) = tags {
                entry.tags = tags;
            }
            let updated = entry.clone();
            drop(entries);
            self.save()?;
            return Ok(updated);
        }
        Err(format!("Memory entry '{}' not found", id))
    }

    /// Delete a memory entry by ID. Returns the removed entry, or `None` if not found.
    pub fn delete(&self, id: &str) -> Result<Option<MemoryEntry>, String> {
        let mut entries = self.entries.lock().unwrap();
        let idx = entries.iter().position(|e| e.id == id);
        let removed = match idx {
            Some(i) => Some(entries.remove(i)),
            None => None,
        };
        drop(entries);
        if removed.is_some() {
            self.save()?;
        }
        Ok(removed)
    }

    /// Look up a memory entry by ID.
    pub fn find(&self, id: &str) -> Option<MemoryEntry> {
        let entries = self.entries.lock().unwrap();
        entries.iter().find(|e| e.id == id).cloned()
    }

    /// Mark an entry as superseded by `new_id`. Both entries must exist.
    pub fn supersede(&self, id: &str, new_id: &str) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        if entries.iter().any(|e| e.id == new_id) {
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.supersedes = Some(new_id.to_string());
                drop(entries);
                self.save()?;
                return Ok(());
            }
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
        let max_entries = self.config().max_entries;
        let entries = self.entries.lock().unwrap();
        let active_count = count_active_memories(&entries);

        if active_count <= max_entries {
            return Ok(());
        }

        // Sort by confidence (ascending) and timestamp (ascending) to evict oldest/lowest first
        let mut sorted: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| !e.is_expired() && e.supersedes.is_none())
            .collect();
        sorted.sort_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let ta = a.timestamp.unwrap_or_default();
                    let tb = b.timestamp.unwrap_or_default();
                    ta.cmp(&tb)
                })
        });

        // Collect IDs to remove BEFORE dropping the lock to avoid deadlock
        let to_remove = active_count - max_entries;
        let ids_to_remove: Vec<String> = sorted
            .iter()
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

    /// Clean up expired and low-quality memories.
    /// Removes expired entries (>90 days old) and entries with very low confidence.
    /// Returns the number of entries removed.
    pub fn cleanup(&self) -> Result<usize, String> {
        let mut entries = self.entries.lock().unwrap();
        let original_count = entries.len();

        let now = chrono::Utc::now();

        entries.retain(|e| {
            // Keep if not expired
            if let Some(ts) = e.timestamp {
                let age_days = now.signed_duration_since(ts).num_days();
                if age_days > 90 {
                    tracing::debug!(
                        "[MEMORY] Cleaning expired memory ({} days old): {}",
                        age_days,
                        e.id
                    );
                    return false;
                }
            }

            // Keep if confidence is reasonable (above 0.3)
            if e.confidence < 0.3 {
                tracing::debug!(
                    "[MEMORY] Cleaning low-confidence memory ({}): {}",
                    e.confidence,
                    e.id
                );
                return false;
            }

            true
        });

        let removed = original_count - entries.len();
        drop(entries);

        if removed > 0 {
            self.save()?;
            tracing::info!(
                "Cleaned up {} memories, {} remaining",
                removed,
                self.count()
            );
        }
        Ok(removed)
    }
}

impl MemoryManager {
    /// Save memories to disk.
    ///
    /// Clones the entries and releases the mutex *before* the filesystem I/O so
    /// concurrent readers/writers are not blocked during a slow disk write.
    pub fn save(&self) -> Result<(), String> {
        let snapshot: Vec<MemoryEntry> = self.entries.lock().unwrap().clone();
        save_memories(&self.storage_path, &snapshot)
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

/// Result of a memory insert attempt.
pub enum MemoryAddResult {
    /// The entry was new and got stored; carries the created entry (with its ID).
    Created(MemoryEntry),
    /// A strong near-duplicate already existed; `existing` is that entry.
    Duplicate(MemoryEntry),
}

/// Token overlap of two memory entries' content: (intersection, jaccard).
fn content_overlap(a: &MemoryEntry, b: &MemoryEntry) -> (usize, f64) {
    let tokens: fn(&MemoryEntry) -> std::collections::HashSet<String> = |e| {
        e.content
            .to_lowercase()
            .split_whitespace()
            .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|t| t.len() >= 2)
            .collect()
    };
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return (0, 0.0);
    }
    let intersection = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    (intersection, intersection as f64 / union as f64)
}

/// Strong-match gate: exact content match, or substantial token overlap.
/// Requires at least 3 shared tokens so very short entries (e.g. "Memory 0"
/// vs "Memory 1") are not collapsed.
fn is_near_duplicate(existing: &MemoryEntry, new: &MemoryEntry) -> bool {
    if existing.content == new.content {
        return true;
    }
    let (intersection, jaccard) = content_overlap(existing, new);
    jaccard >= 0.8 && intersection >= 3
}

/// Remove expired (>90 days) and low-confidence (<0.3) entries.
/// Returns the number of entries removed.
fn clean_stale_entries(entries: &mut Vec<MemoryEntry>) -> usize {
    let original_count = entries.len();
    let now = chrono::Utc::now();
    entries.retain(|e| {
        if let Some(ts) = e.timestamp {
            if now.signed_duration_since(ts).num_days() > 90 {
                return false;
            }
        }
        e.confidence >= 0.3
    });
    original_count - entries.len()
}

/// I4: persisted state of the last auto-improvement check (unix seconds).
/// Private: only `record_improvement_check` / `has_new_improvement_evidence`
/// touch it.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Copy, Debug, PartialEq)]
struct ImprovementState {
    /// Unix timestamp (seconds) of the last improvement check, if any.
    #[serde(default)]
    last_check: Option<i64>,
}

/// Load the improvement-check state, tolerating a missing or corrupt file
/// (both mean "no check recorded yet").
fn load_improvement_state(path: &std::path::Path) -> ImprovementState {
    if !path.exists() {
        return ImprovementState::default();
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
