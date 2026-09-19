use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tracing;

use crate::types::Message;

use super::types::{MemoryConfig, MemoryEntry, MemoryType};
use super::search::get_recent_memories;
use super::search::search_memories;
use super::storage::{load_memories, save_memories, get_memories_path, count_active_memories};
use super::improver::suggest_improvements;
use crate::llm::LlmClient;

/// Main orchestrator for the memory system.
///
/// `config` is interior-mutable so the UI (which holds an `Arc<MemoryManager>`)
/// can update memory settings at runtime without a `&mut` borrow.
pub struct MemoryManager {
    entries: Arc<Mutex<Vec<MemoryEntry>>>,
    config: Mutex<MemoryConfig>,
    storage_path: PathBuf,
    llm_client: Option<Arc<dyn LlmClient>>,
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
    pub fn new_with_llm(config: MemoryConfig, llm_client: Arc<dyn LlmClient>) -> Result<Self, String> {
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
        let config = self.config();
        if !config.enabled {
            return Vec::new();
        }

        let entries = self.entries.lock().unwrap();
        search_memories(&*entries, query, &config).into_iter().map(|e| (*e).clone()).collect()
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
            entries.iter()
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
    pub fn update(&self, id: &str, content: &str, tags: Option<Vec<String>>) -> Result<MemoryEntry, String> {
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
        let to_remove = active_count - max_entries;
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
                    tracing::debug!("[MEMORY] Cleaning expired memory ({} days old): {}", age_days, e.id);
                    return false;
                }
            }

            // Keep if confidence is reasonable (above 0.3)
            if e.confidence < 0.3 {
                tracing::debug!("[MEMORY] Cleaning low-confidence memory ({}): {}", e.confidence, e.id);
                return false;
            }

            true
        });

        let removed = original_count - entries.len();
        drop(entries);

        if removed > 0 {
            self.save()?;
            tracing::info!("Cleaned up {} memories, {} remaining", removed, self.count());
        }
        Ok(removed)
    }

    /// Run a full LLM memory-maintenance pass over the whole store.
    ///
    /// The store is processed in batches of `memory_maintenance_batch_size`
    /// (oldest entries first) so every LLM call is small and bounded — each
    /// batch is one non-streaming prompt, actions are applied before the next
    /// batch runs, and the returned report aggregates all batches. A batch
    /// failure (e.g. its timeout firing) is logged and the sweep continues.
    ///
    /// Per-batch safety checks:
    /// - unknown IDs are skipped
    /// - consolidated content is capped in length
    /// - no batch can ever merge/delete the entire (global) store
    ///
    /// Returns `Ok(report)` with `summary` describing what happened.
    /// Returns `Err` when no LLM client is attached.
    pub async fn run_maintenance(&self) -> Result<MaintenanceReport, String> {
        self.run_maintenance_full(None).await
    }

    /// Same as [`Self::run_maintenance`], but reports live batch progress via
    /// `progress` (1-based current batch / total batches) so a UI can show
    /// "step i/M" while the pass runs on a helper thread.
    pub async fn run_maintenance_full(
        &self,
        progress: Option<MaintenanceProgress>,
    ) -> Result<MaintenanceReport, String> {
        if self.llm_client.is_none() {
            return Err("No LLM client attached for memory maintenance".to_string());
        }
        let mut entries = self.get_all_memories();
        if entries.len() < 2 {
            let report = MaintenanceReport::summary("Maintenance skipped: fewer than 2 entries");
            tracing::info!("[MEMORY] {}", report.summary);
            return Ok(report);
        }
        // Oldest first: stale/overlapping entries consolidate early, and a
        // manual sweep makes forward progress on the oldest part of the store.
        entries.sort_by_key(|e| e.timestamp.unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC));
        let batch_size = self.config().memory_maintenance_batch_size.clamp(5, 100);
        // A single leftover entry cannot be merged with anything; skip it.
        let chunks: Vec<&[MemoryEntry]> = entries
            .chunks(batch_size)
            .filter(|c| c.len() >= 2)
            .collect();
        let total_batches = chunks.len();
        if let Some(p) = &progress {
            p.total.store(total_batches, Ordering::SeqCst);
        }

        let mut merges = 0;
        let mut updated = 0;
        let mut deleted = 0;
        let mut had_actions = false;
        let mut failures = 0;
        for (i, chunk) in chunks.iter().enumerate() {
            let n = i + 1;
            if let Some(p) = &progress {
                p.current.store(n, Ordering::SeqCst);
            }
            match self.run_maintenance_batch(*chunk, entries.len()).await {
                Ok(r) => {
                    merges += r.merges;
                    updated += r.updated;
                    deleted += r.deleted;
                    had_actions = had_actions || r.had_actions;
                }
                Err(e) => {
                    failures += 1;
                    tracing::warn!("[MEMORY] Maintenance batch {n}/{total_batches} failed: {e}");
                }
            }
        }

        let summary = if merges + updated + deleted == 0 {
            if had_actions {
                "Maintenance complete: no changes applied".to_string()
            } else {
                "Maintenance complete: no changes suggested".to_string()
            }
        } else {
            let mut parts = Vec::new();
            if merges > 0 {
                parts.push(format!("{merges} merged"));
            }
            if updated > 0 {
                parts.push(format!("{updated} updated"));
            }
            if deleted > 0 {
                parts.push(format!("{deleted} deleted"));
            }
            format!("Maintenance complete: {} ({} batches)", parts.join(", "), total_batches)
        };
        let report = MaintenanceReport {
            summary,
            batches: total_batches,
            merges,
            updated,
            deleted,
            had_actions,
        };
        if failures > 0 {
            tracing::warn!("[MEMORY] {failures} maintenance batch(es) failed; see earlier log lines");
        }
        tracing::info!("[MEMORY] {}", report.summary);
        Ok(report)
    }

    /// Run exactly ONE maintenance batch over the oldest
    /// `memory_maintenance_batch_size` active entries.
    ///
    /// Used by the post-task engine path so every step stays small and fast;
    /// repeated tasks make forward progress on the oldest entries of the
    /// store. Returns `Err` when no LLM client is attached.
    pub async fn run_maintenance_step(&self) -> Result<MaintenanceReport, String> {
        if self.llm_client.is_none() {
            return Err("No LLM client attached for memory maintenance".to_string());
        }
        let mut entries = self.get_all_memories();
        if entries.len() < 2 {
            return Ok(MaintenanceReport::summary(
                "Maintenance step skipped: fewer than 2 entries",
            ));
        }
        entries.sort_by_key(|e| e.timestamp.unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC));
        let batch_size = self.config().memory_maintenance_batch_size.clamp(5, 100);
        let chunk: Vec<MemoryEntry> = entries.drain(..batch_size.min(entries.len())).collect();
        // `entries` is the remainder after draining: the pre-step global
        // count (for the never-wipe check) is chunk + remainder.
        let report = self
            .run_maintenance_batch(&chunk, chunk.len() + entries.len())
            .await?;
        tracing::info!("[MEMORY] {}", report.summary);
        Ok(report)
    }

    /// One maintenance batch: a single small LLM call covering only `chunk`,
    /// followed by applying the returned actions.
    ///
    /// `total_entries` is the size of the WHOLE store (not the batch) so the
    /// "never let one merge wipe the store" check stays globally correct.
    async fn run_maintenance_batch(
        &self,
        chunk: &[MemoryEntry],
        total_entries: usize,
    ) -> Result<MaintenanceReport, String> {
        let llm = match &self.llm_client {
            Some(c) => c.clone(),
            None => return Err("No LLM client attached for memory maintenance".to_string()),
        };

        let mut lines = Vec::new();
        for e in chunk {
            let age = e
                .timestamp
                .map(|ts| format!("{}d old", chrono::Utc::now().signed_duration_since(ts).num_days().max(0)))
                .unwrap_or_default();
            lines.push(format!(
                "- id: {} | type: {} | tags: [{}] | age: {} | {}",
                e.id,
                e.r#type,
                e.tags.join(", "),
                age,
                e.content
            ));
        }
        let prompt = format!(
            "You are maintaining a memory store for an AI agent. Below are a batch of stored memory entries (the store is maintained in batches).\n\
             Identify duplicates, stale or contradictory entries, and overlapping information within this batch.\n\
             \n\
             Memory entries:\n{}\n\
             \n\
             Return a JSON object with optional keys (omit empty arrays):\n\
             {{\n\
             \"merge\": [{{\"ids\": [\"id1\", \"id2\"], \"consolidated\": \"merged content\", \"tags\": [\"tag\"]}}],\n\
             \"update\": [{{\"id\": \"id1\", \"content\": \"improved content\", \"tags\": [\"tag\"]}}],\n\
             \"delete\": [{{\"id\": \"id1\", \"reason\": \"why\"}}]\n\
             }}\n\
             Rules: only merge entries that are genuinely duplicates or heavily overlapping; \
             never delete the only entry covering a topic; prefer update over delete when in doubt; \
             return an empty object {{}} if no changes are needed.",
            lines.join("\n")
        );
        let messages = vec![Message {
            role: "user".to_string(),
            content: prompt,
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }];

        // Per-step timeout: `memory_maintenance_timeout_secs` bounds this
        // single batch (LLM call), so one slow batch can neither stall a
        // multi-batch sweep nor hold a task completion hostage.
        let per_step = std::time::Duration::from_secs(
            self.config().memory_maintenance_timeout_secs.max(10),
        );
        let response = tokio::time::timeout(per_step, llm.complete(&messages))
            .await
            .map_err(|_| {
                format!(
                    "Maintenance batch timed out after {}s (raise the step limit in Settings)",
                    per_step.as_secs()
                )
            })?
            .map_err(|e| {
                tracing::warn!("[MEMORY] Maintenance LLM call failed: {}", e);
                format!("Maintenance LLM call failed: {}", e)
            })?;
        let actions = parse_maintenance_actions(&response);
        if actions.is_empty() {
            Ok(MaintenanceReport::summary("Maintenance complete: no changes suggested"))
        } else {
            tracing::info!("[MEMORY] Applying maintenance: {} merges, {} updates, {} deletes",
                actions.merge.len(), actions.update.len(), actions.delete.len());
            Ok(apply_maintenance_actions(self, chunk, total_entries, &actions))
        }
    }

    /// Save memories to disk.
    ///
    /// Clones the entries and releases the mutex *before* the filesystem I/O so
    /// concurrent readers/writers are not blocked during a slow disk write.
    pub fn save(&self) -> Result<(), String> {
        let snapshot: Vec<MemoryEntry> = self.entries.lock().unwrap().clone();
        save_memories(&self.storage_path, &snapshot)
    }

    /// Build the memory context block for injection into system prompts.
    pub fn build_context_block(&self, query: &str) -> String {
        let config = self.config();
        if !config.enabled || config.injection_mode == super::types::InjectionMode::Off {
            return String::new();
        }

        let memories = if query.trim().is_empty() {
            // No query available: fall back to the most recent entries.
            self.get_recent(config.injection_max_entries)
        } else {
            let results = self.search(query);
            if results.is_empty() {
                match config.injection_mode {
                    // Smart mode: inject only when there are relevant hits.
                    super::types::InjectionMode::Smart => return String::new(),
                    // Always mode: fall back to the 3 most recent entries.
                    super::types::InjectionMode::Always => return self.fallback_recent_block(3),
                    super::types::InjectionMode::Off => unreachable!(),
                }
            }
            results
        };
        self.render_block(memories)
    }

    /// Render a block from the given entries (shared by the main path and the
    /// always-mode recent fallback).
    fn render_block(&self, memories: Vec<MemoryEntry>) -> String {
        if memories.is_empty() {
            return String::new();
        }

        let max_chars = self.config().injection_max_chars;
        let mut block = String::from("\n═══ MEMORY CONTEXT ═══\n(Relevant memories from past sessions)\n\n");
        let mut chars = 0;

        for memory in &memories {
            let line = format!("[{}] {}\n", memory.r#type, memory.content);
            if chars + line.len() > max_chars {
                break;
            }
            block.push_str(&line);
            chars += line.len();
        }

        block.push_str("══════════════════════\n");
        block
    }

    /// Always-mode fallback block built from the N most recent entries.
    fn fallback_recent_block(&self, count: usize) -> String {
        self.render_block(self.get_recent(count))
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

/// A human-readable report of a memory-maintenance pass (or a single batch
/// step). Counts are used to aggregate multi-batch passes.
#[derive(Clone, Debug)]
pub struct MaintenanceReport {
    /// Human-readable summary of what happened.
    pub summary: String,
    /// Number of batches this pass covered (1 for a single-batch step).
    pub batches: usize,
    /// Total merge actions applied (one action may combine >2 source entries).
    pub merges: usize,
    /// Total entries updated.
    pub updated: usize,
    /// Total entries deleted.
    pub deleted: usize,
    /// Whether the LLM suggested any actions (they may all have been skipped).
    pub had_actions: bool,
}

impl MaintenanceReport {
    fn summary(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            batches: 1,
            merges: 0,
            updated: 0,
            deleted: 0,
            had_actions: false,
        }
    }
}

/// Live progress of a batched maintenance pass, shared with the UI (the pass
/// runs on a helper thread). Both counters are 0 until the pass starts
/// (`total`) / begins its first batch (`current`).
#[derive(Clone, Default)]
pub struct MaintenanceProgress {
    /// 1-based index of the batch currently running.
    pub current: Arc<AtomicUsize>,
    /// Total batches in this pass.
    pub total: Arc<AtomicUsize>,
}

/// Actions requested by the LLM during a maintenance pass.
#[derive(serde::Deserialize, Default, Debug)]
struct MaintenanceActions {
    #[serde(default)]
    merge: Vec<MergeAction>,
    #[serde(default)]
    update: Vec<UpdateAction>,
    #[serde(default)]
    delete: Vec<DeleteAction>,
}

impl MaintenanceActions {
    fn is_empty(&self) -> bool {
        self.merge.is_empty() && self.update.is_empty() && self.delete.is_empty()
    }
}

#[derive(serde::Deserialize, Debug)]
struct MergeAction {
    ids: Vec<String>,
    #[serde(default)]
    consolidated: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(serde::Deserialize, Debug)]
struct UpdateAction {
    id: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(serde::Deserialize, Debug)]
struct DeleteAction {
    id: String,
    #[serde(default)]
    reason: String,
}

const MAX_CONSOLIDATED_CHARS: usize = 2000;

/// Parse the LLM's maintenance response into actions, tolerating JSON
/// wrapped in code fences or surrounded by prose.
fn parse_maintenance_actions(response: &str) -> MaintenanceActions {
    let trimmed = response.trim();
    let json = if trimmed.starts_with('{') {
        trimmed.to_string()
    } else {
        let start = trimmed.find('{');
        let end = trimmed.rfind('}');
        match (start, end) {
            (Some(s), Some(e)) if e > s => trimmed[s..=e].to_string(),
            _ => return MaintenanceActions::default(),
        }
    };
    serde_json::from_str(&json).unwrap_or_else(|e| {
        tracing::warn!("[MEMORY] Failed to parse maintenance actions: {}", e);
        MaintenanceActions::default()
    })
}

/// Apply the parsed actions to the store, with safety checks.
///
/// `entries` is the batch snapshot used to validate IDs and source content;
/// `total_entries` is the size of the WHOLE store, used to enforce "never
/// wipe everything" globally (a batch is usually a subset of the store).
fn apply_maintenance_actions(
    manager: &MemoryManager,
    entries: &[MemoryEntry],
    total_entries: usize,
    actions: &MaintenanceActions,
) -> MaintenanceReport {
    let existing_ids: std::collections::HashSet<&str> =
        entries.iter().map(|e| e.id.as_str()).collect();
    let mut merged: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    // Merge: create one consolidated entry, then delete the sources.
    for merge in &actions.merge {
        if merge.ids.len() < 2 {
            continue;
        }
        let valid: Vec<&String> = merge
            .ids
            .iter()
            .filter(|id| existing_ids.contains(id.as_str()))
            .collect();
        if valid.len() < 2 {
            skipped.push(format!("merge needs at least 2 known ids (got {})", merge.ids.len()));
            continue;
        }
        // Safety: never let a single merge consume the entire store
        // (checked against the global store size, not this batch).
        if total_entries > 0 && valid.len() >= total_entries {
            skipped.push(format!("merge of {} entries would remove the whole store", valid.len()));
            continue;
        }
        let mut content = merge.consolidated.trim().to_string();
        if content.is_empty() {
            // Fall back to concatenating the source contents.
            content = valid
                .iter()
                .filter_map(|id| entries.iter().find(|e| &e.id == *id))
                .map(|e| e.content.clone())
                .collect::<Vec<_>>()
                .join("; ");
        }
        if content.len() > MAX_CONSOLIDATED_CHARS {
            content.truncate(MAX_CONSOLIDATED_CHARS);
        }
        let mut tags: std::collections::VecDeque<String> = tags_from_sources(entries, &valid);
        for t in &merge.tags {
            if !tags.contains(t) {
                tags.push_back(t.clone());
            }
        }
        let entry = MemoryEntry::new(
            first_source_type(entries, &valid),
            &content,
            "maintenance",
            &tags.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        match manager.add(entry) {
            Ok(MemoryAddResult::Created(c)) => {
                for id in &valid {
                    let _ = manager.delete(id);
                    deleted.push(id.to_string());
                }
                merged.push(c.id.clone());
            }
            Ok(MemoryAddResult::Duplicate(_)) => {
                // Consolidated content already covered by an existing entry:
                // still collapse the sources into it.
                for id in &valid {
                    let _ = manager.delete(id);
                    deleted.push(id.to_string());
                }
                merged.push("existing".to_string());
            }
            Err(e) => {
                skipped.push(format!("merge failed to save: {}", e));
            }
        }
    }

    // Update: refresh content/tags, reviving superseded entries.
    for update in &actions.update {
        if !existing_ids.contains(update.id.as_str()) {
            skipped.push(format!("update skipped, unknown id {}", update.id));
            continue;
        }
        if update.content.trim().is_empty() {
            continue;
        }
        let tags = if update.tags.is_empty() {
            None
        } else {
            Some(update.tags.clone())
        };
        if let Err(e) = manager.update(&update.id, update.content.trim(), tags) {
            skipped.push(format!("update of {} failed: {}", update.id, e));
        }
    }

    // Delete: drop stale entries, but never the last remaining entry.
    let mut current_count = manager.count();
    for delete in actions.delete.iter().filter(|d| existing_ids.contains(d.id.as_str())) {
        if current_count <= 1 {
            skipped.push("delete skipped, would remove the last entry".into());
            continue;
        }
        let reason = if delete.reason.trim().is_empty() {
            String::new()
        } else {
            format!(" ({})", delete.reason.trim())
        };
        match manager.delete(&delete.id) {
            Ok(Some(_)) => {
                deleted.push(delete.id.clone());
                tracing::debug!("[MEMORY] Maintenance delete {}:{}", delete.id, reason);
                current_count = current_count.saturating_sub(1);
            }
            Ok(None) => skipped.push(format!("delete skipped, unknown id {}", delete.id)),
            Err(e) => skipped.push(format!("delete of {} failed: {}", delete.id, e)),
        }
    }

    let mut parts = Vec::new();
    if !merged.is_empty() {
        parts.push(format!("{} merged", merged.len()));
    }
    let updates_applied = actions
        .update
        .iter()
        .filter(|u| existing_ids.contains(u.id.as_str()) && !u.content.trim().is_empty())
        .count();
    if updates_applied > 0 {
        parts.push(format!("{} updated", updates_applied));
    }
    if !deleted.is_empty() {
        parts.push(format!("{} deleted", deleted.len()));
    }
    let summary = if parts.is_empty() {
        "Maintenance complete: no changes applied".to_string()
    } else {
        format!("Maintenance complete: {}", parts.join(", "))
    };
    if !skipped.is_empty() {
        tracing::debug!("[MEMORY] Maintenance skipped actions: {:?}", skipped);
    }
    MaintenanceReport {
        summary,
        batches: 1,
        merges: merged.len(),
        updated: updates_applied,
        deleted: deleted.len(),
        had_actions: true,
    }
}

fn first_source_type(entries: &[MemoryEntry], ids: &[&String]) -> MemoryType {
    ids.iter()
        .filter_map(|id| entries.iter().find(|e| &e.id == *id))
        .map(|e| e.r#type.clone())
        .next()
        .unwrap_or(MemoryType::Fact)
}

fn tags_from_sources(entries: &[MemoryEntry], ids: &[&String]) -> std::collections::VecDeque<String> {
    let mut tags = std::collections::VecDeque::new();
    for id in ids {
        if let Some(e) = entries.iter().find(|e| &e.id == *id) {
            for t in &e.tags {
                if !tags.contains(t) {
                    tags.push_back(t.clone());
                }
            }
        }
    }
    tags
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

#[cfg(test)]
mod tests;
