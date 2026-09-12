use std::path::PathBuf;
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

    /// Run an LLM memory-maintenance pass.
    ///
    /// Sends all active entries to the LLM and applies the returned JSON
    /// actions (`merge`, `update`, `delete`) with safety checks:
    /// - unknown IDs are skipped
    /// - consolidated content is capped in length
    /// - a single pass can never merge/delete all entries
    ///
    /// Returns `Ok(report)` with `summary` describing what happened.
    /// Returns `Err` when no LLM client is attached.
    pub async fn run_maintenance(&self) -> Result<MaintenanceReport, String> {
        let llm = match &self.llm_client {
            Some(c) => c.clone(),
            None => return Err("No LLM client attached for memory maintenance".to_string()),
        };

        let entries = self.get_all_memories();
        let report = if entries.len() < 2 {
            MaintenanceReport::summary("Maintenance skipped: fewer than 2 entries")
        } else {
            let mut lines = Vec::new();
            for e in &entries {
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
                "You are maintaining a memory store for an AI agent. Below are all stored memory entries.\n\
                 Identify duplicates, stale or contradictory entries, and overlapping information.\n\
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
            }];

            let response = llm.complete(&messages).await.map_err(|e| {
                tracing::warn!("[MEMORY] Maintenance LLM call failed: {}", e);
                format!("Maintenance LLM call failed: {}", e)
            })?;
            let actions = parse_maintenance_actions(&response);
            if actions.is_empty() {
                MaintenanceReport::summary("Maintenance complete: no changes suggested")
            } else {
                tracing::info!("[MEMORY] Applying maintenance: {} merges, {} updates, {} deletes",
                    actions.merge.len(), actions.update.len(), actions.delete.len());
                apply_maintenance_actions(self, &entries, &actions)
            }
        };
        tracing::info!("[MEMORY] {}", report.summary);
        Ok(report)
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

/// A human-readable report of a memory-maintenance pass.
pub struct MaintenanceReport {
    pub summary: String,
}

impl MaintenanceReport {
    fn summary(summary: impl Into<String>) -> Self {
        Self { summary: summary.into() }
    }
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
/// `entries` is the pre-pass snapshot (from `get_all_memories`) used to
/// validate IDs and enforce "never wipe everything".
fn apply_maintenance_actions(
    manager: &MemoryManager,
    entries: &[MemoryEntry],
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
        // Safety: never let a single merge consume the entire store.
        if valid.len() >= entries.len() {
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
    MaintenanceReport { summary }
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
    fn test_add_dedup_identical() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        let first = MemoryEntry::new(
            MemoryType::Fact,
            "The build fails on Windows because of a missing semicolon in config.rs",
            "test",
            &["build"],
        );
        let first_id = first.id.clone();
        match manager.add(first).unwrap() {
            MemoryAddResult::Created(e) => assert_eq!(e.id, first_id),
            MemoryAddResult::Duplicate(_) => panic!("first add should be Created"),
        }

        // Identical content + type -> duplicate, not inserted.
        let dup = MemoryEntry::new(
            MemoryType::Fact,
            "The build fails on Windows because of a missing semicolon in config.rs",
            "test",
            &["build"],
        );
        match manager.add(dup).unwrap() {
            MemoryAddResult::Duplicate(existing) => assert_eq!(existing.id, first_id),
            MemoryAddResult::Created(_) => panic!("identical content should be Duplicate"),
        }
        assert_eq!(manager.count(), 1);
    }

    #[test]
    fn test_add_dedup_near_duplicate() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "Always run cargo test after modifying the memory module in this repository",
                "test",
                &["testing"],
            ))
            .unwrap();

        // Reworded but same meaning -> high token overlap -> duplicate.
        match manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "Always run cargo test after modifying the memory module in this repository, it catches regressions",
                "test",
                &["testing"],
            ))
            .unwrap()
        {
            MemoryAddResult::Duplicate(_) => {}
            MemoryAddResult::Created(_) => panic!("near-duplicate should be rejected"),
        }
        assert_eq!(manager.count(), 1);
    }

    #[test]
    fn test_add_distinct_contents() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        manager
            .add(MemoryEntry::new(
                MemoryType::Fact,
                "The project uses a Rust Cargo workspace with core and egui crates",
                "test",
                &["architecture"],
            ))
            .unwrap();
        match manager
            .add(MemoryEntry::new(
                MemoryType::Decision,
                "We decided to store sessions as encrypted JSON files with a WUFFENC prefix",
                "test",
                &["sessions"],
            ))
            .unwrap()
        {
            MemoryAddResult::Created(_) => {}
            MemoryAddResult::Duplicate(_) => panic!("distinct content should be Created"),
        }
        assert_eq!(manager.count(), 2);
    }

    #[test]
    fn test_get_recent_skips_superseded() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        let old = MemoryEntry::new(MemoryType::Fact, "Old superseded memory", "test", &[]);
        let old_id = old.id.clone();
        manager.add(old).unwrap();

        // Newer entry marks the old one as superseded.
        let new = MemoryEntry::new(MemoryType::Fact, "Newer replacement memory", "test", &[]);
        let new_id = new.id.clone();
        manager.add(new).unwrap();
        manager.supersede(&old_id, &new_id).unwrap();

        let recent = manager.get_recent(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, new_id, "get_recent must skip superseded entries");
    }

    #[test]
    fn test_delete_missing_returns_none() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        assert!(manager.delete("nonexistent-id").unwrap().is_none());
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn test_update_replaces_tags_and_revives() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        let e = MemoryEntry::new(MemoryType::Fact, "Original content here for testing purposes", "test", &["old"]);
        let id = e.id.clone();
        manager.add(e).unwrap();

        let updated = manager.update(&id, "Refined content here for testing purposes", Some(vec!["new".to_string()])).unwrap();
        assert_eq!(updated.tags, vec!["new"]);
        assert_eq!(updated.supersedes, None);

        // Superseded entries are revived by update.
        let e2 = MemoryEntry::new(MemoryType::Fact, "Another original memory for the revival test", "test", &[]);
        let id2 = e2.id.clone();
        manager.add(e2).unwrap();
        manager.supersede(&id2, &id).unwrap();
        let revived = manager.update(&id2, "Revived memory content for the revival test now", None).unwrap();
        assert_eq!(revived.supersedes, None);
        assert_eq!(revived.tags, Vec::<String>::new(), "tags unchanged when None is passed");
        assert!(manager.get_recent(10).iter().any(|m| m.id == id2));
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
            "Shell tool plan was implemented with nested config",
            "test",
            &["shell"],
        )).unwrap();

        let block = manager.build_context_block("rust project");
        assert!(block.contains("WuffAgent is a Rust project"));
        assert!(block.contains("═══ MEMORY CONTEXT ═══"));
    }

    #[test]
    fn test_query_injection_prefers_relevant_hit() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            injection_max_entries: 1,
            injection_max_chars: 500,
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();

        manager.add(MemoryEntry::new(
            MemoryType::Fact,
            "The deploy script lives in scripts/deploy.sh",
            "test",
            &["deploy"],
        )).unwrap();
        manager.add(MemoryEntry::new(
            MemoryType::Fact,
            "WuffAgent is a Rust project using eframe",
            "test",
            &["project"],
        )).unwrap();

        // Query about the deploy script should surface the deploy entry first.
        let block = manager.build_context_block("run the deploy script");
        assert!(block.contains("scripts/deploy.sh"), "relevant entry must be injected: {block}");
        assert!(!block.contains("eframe"), "unrelated entry must not fill the single slot: {block}");
    }

    #[test]
    fn test_query_injection_empty_store_fallback() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            injection_max_entries: 5,
            injection_max_chars: 500,
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            injection_mode: super::super::types::InjectionMode::Always,
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();
        manager
            .add(MemoryEntry::new(
                MemoryType::Fact,
                "Recent unrelated architecture note",
                "test",
                &[],
            ))
            .unwrap();

        // Query with no hits: Always mode falls back to the most recent entry.
        let block = manager.build_context_block("quantum entanglement theory");
        assert!(block.contains("Recent unrelated architecture note"), "recent fallback expected: {block}");

        // Smart mode with no hits: nothing is injected.
        manager.set_config(MemoryConfig {
            injection_mode: super::super::types::InjectionMode::Smart,
            ..manager.config().clone()
        });
        assert_eq!(manager.build_context_block("quantum entanglement theory"), "");
    }

    // --- Maintenance pass tests ---

    /// Mock LLM returning a fixed maintenance-actions JSON response.
    struct ScriptedLlm {
        response: String,
    }

    #[async_trait::async_trait]
    impl LlmClient for ScriptedLlm {
        async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
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

    fn manager_with_llm(dir: &std::path::Path, response: &str) -> MemoryManager {
        let config = MemoryConfig {
            memories_dir: Some(dir.to_str().unwrap().to_string()),
            memory_maintenance: true,
            ..Default::default()
        };
        MemoryManager::new_with_llm(config, Arc::new(ScriptedLlm { response: response.to_string() })).unwrap()
    }

    /// Build an entry with a known ID so scripted LLM responses can target it.
    fn entry_with_id(id: &str, r#type: MemoryType, content: &str, tags: &[&str]) -> MemoryEntry {
        let mut e = MemoryEntry::new(r#type, content, "test", tags);
        e.id = id.to_string();
        e
    }

    /// Seed the standard three-entry fixture into `manager`.
    fn seed_three(manager: &MemoryManager) {
        for e in [
            entry_with_id("mem-a", MemoryType::Fact, "The build uses cargo with a workspace layout for core and gui", &["build"]),
            entry_with_id("mem-b", MemoryType::Fact, "The release notes must mention the new memory panel feature", &["release"]),
            entry_with_id("mem-c", MemoryType::Lesson, "Use PowerShell not bash when running commands on this machine", &["shell"]),
        ] {
            manager.add(e).unwrap();
        }
    }

    #[tokio::test]
    async fn test_maintenance_merge() {
        let dir = tempdir().unwrap();
        let response = r#"{"merge": [{"ids": ["mem-a", "mem-b"], "consolidated": "Consolidated build and release note"}]}"#;
        let manager = manager_with_llm(dir.path(), response);
        seed_three(&manager);

        let report = manager.run_maintenance().await.unwrap();
        assert!(report.summary.contains("merged"), "summary: {}", report.summary);
        assert!(manager.find("mem-a").is_none(), "source a must be removed");
        assert!(manager.find("mem-b").is_none(), "source b must be removed");
        assert!(manager.find("mem-c").is_some(), "unrelated entry must survive");
        // The consolidated entry replaced the two sources: net -1.
        assert_eq!(manager.count(), 2);
    }

    #[tokio::test]
    async fn test_maintenance_update_and_delete() {
        let dir = tempdir().unwrap();
        let response = r#"{"update": [{"id": "mem-a", "content": "Updated build note content", "tags": ["build", "v2"]}], "delete": [{"id": "mem-b", "reason": "stale"}]}"#;
        let manager = manager_with_llm(dir.path(), response);
        seed_three(&manager);

        let report = manager.run_maintenance().await.unwrap();
        assert!(report.summary.contains("updated"), "summary: {}", report.summary);
        assert!(report.summary.contains("deleted"), "summary: {}", report.summary);

        let updated = manager.find("mem-a").unwrap();
        assert_eq!(updated.content, "Updated build note content");
        assert!(updated.tags.contains(&"v2".to_string()));
        assert!(manager.find("mem-b").is_none(), "deleted entry must be gone");
        assert!(manager.find("mem-c").is_some());
    }

    #[tokio::test]
    async fn test_maintenance_unknown_and_malformed_actions() {
        let dir = tempdir().unwrap();
        // Malformed JSON parses to no actions: a no-op, not an error.
        let manager = manager_with_llm(dir.path(), "not json at all");
        seed_three(&manager);
        let report = manager.run_maintenance().await.unwrap();
        assert_eq!(manager.count(), 3);
        assert!(report.summary.contains("no changes suggested"), "summary: {}", report.summary);

        // Unknown IDs in every action type: no-op.
        let response = r#"{"merge": [{"ids": ["nope1", "nope2"], "consolidated": "x"}], "update": [{"id": "nope3", "content": "y"}], "delete": [{"id": "nope4", "reason": "r"}]}"#;
        let manager = manager_with_llm(dir.path(), response);
        seed_three(&manager);
        let report = manager.run_maintenance().await.unwrap();
        assert_eq!(manager.count(), 3, "unknown ids must not mutate the store");
        assert!(report.summary.contains("no changes applied"), "summary: {}", report.summary);
    }

    #[tokio::test]
    async fn test_maintenance_never_wipes_store() {
        let dir = tempdir().unwrap();
        // Delete all three: the last remaining one must be kept.
        let response = r#"{"delete": [{"id": "mem-a"}, {"id": "mem-b"}, {"id": "mem-c"}]}"#;
        let manager = manager_with_llm(dir.path(), response);
        seed_three(&manager);

        let report = manager.run_maintenance().await.unwrap();
        assert_eq!(manager.count(), 1, "store must never be emptied: {}", report.summary);
    }

    #[tokio::test]
    async fn test_maintenance_requires_llm_client() {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();
        assert!(manager.run_maintenance().await.is_err());
    }
}