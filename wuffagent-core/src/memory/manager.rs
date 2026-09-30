use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing;

// The auto-improvement loop's half of the manager (check wrappers, persisted
// per-agent state, evidence gate) lives in the `improvement` child module;
// near-duplicate detection for inserts in `dedup`.
mod dedup;
mod improvement;

pub use dedup::MemoryAddResult;
use self::dedup::is_near_duplicate;

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
    /// Shared LLM-activity tracker (status bar; None = no activity events):
    /// maintenance + improvement checks label their LLM calls ("memory",
    /// "improvement").
    pub(crate) activity: Option<Arc<crate::activity::ActivityTracker>>,
}

impl Clone for MemoryManager {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            config: Mutex::new(self.config()),
            storage_path: self.storage_path.clone(),
            llm_client: self.llm_client.clone(),
            activity: self.activity.clone(),
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
            activity: None,
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

    /// Shared LLM-activity tracker (status bar): maintenance + improvement
    /// checks stream their LLM calls under "memory" / "improvement".
    pub fn with_activity(
        mut self,
        activity: Arc<crate::activity::ActivityTracker>,
    ) -> Self {
        self.activity = Some(activity);
        self
    }

    /// Get a clone of the current config.
    pub fn config(&self) -> MemoryConfig {
        self.config.lock().unwrap().clone()
    }

    /// Replace the config at runtime (used by the settings UI).
    pub fn set_config(&self, config: MemoryConfig) {
        *self.config.lock().unwrap() = config;
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
    ///
    /// Each entry runs through the same near-duplicate gate as
    /// [`Self::add`] ([`is_near_duplicate`]) — checked against the existing
    /// store AND earlier entries of this batch — and duplicates are skipped.
    /// Returns the number of entries actually added.
    pub fn add_batch(&self, entries: Vec<MemoryEntry>) -> Result<usize, String> {
        let mut guard = self.entries.lock().unwrap();
        let mut added = 0;
        for entry in entries {
            if guard.iter().any(|existing| is_near_duplicate(existing, &entry)) {
                continue;
            }
            guard.push(entry);
            added += 1;
        }
        drop(guard);

        if added > 0 {
            self.evict_if_needed()?;
            self.save()?;
        }
        Ok(added)
    }

    /// Update an existing memory entry by ID.
    ///
    /// `content` is optional: when `None` the entry's existing content is
    /// kept (useful for retagging or reviving an entry without touching its
    /// text). Updating an entry revives it (clears `supersedes`) and, when
    /// provided, replaces its tags. Returns the updated entry.
    pub fn update(
        &self,
        id: &str,
        content: Option<&str>,
        tags: Option<Vec<String>>,
    ) -> Result<MemoryEntry, String> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            if let Some(content) = content {
                entry.content = content.to_string();
            }
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

#[cfg(test)]
mod tests;
