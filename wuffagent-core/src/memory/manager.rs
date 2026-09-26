use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing;

use crate::agents::improvement::suggest_improvements;
use super::search::get_recent_memories;
use super::search::search_memories;
use super::storage::{count_active_memories, get_memories_path, load_memories, save_memories};
use super::types::{ImprovementStatus, MemoryConfig, MemoryEntry, MemoryType};
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
        let last_check = self
            .load_state_doc()
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

    /// I4 (cost control): record that an (agent-agnostic) improvement check
    /// just ran, so the global evidence gate stays closed until new Lesson
    /// entries arrive. Kept for the global status view and legacy v1
    /// semantics; the per-task path uses `record_agent_improvement_check`.
    ///
    /// Best-effort: any failure is only logged — the state file must never
    /// be able to break task completion.
    pub fn record_improvement_check(&self) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        doc.last_check = Some(chrono::Utc::now().timestamp());
        self.save_state_doc(&doc);
    }

    // ── 2a: per-agent improvement state ────────────────────────────────────

    /// 2a: this agent's improvement-loop state. The legacy v1 global
    /// `last_check` acts as a fallback BASELINE (evidence gate) for agents
    /// that never had a check of their own; `runs_since_check` and friends
    /// are per-agent only.
    pub fn agent_improvement_state(&self, agent: &str) -> crate::memory::types::AgentImprovementState {
        let doc = self.load_state_doc();
        let rec = doc.agents.get(agent).cloned().unwrap_or_default();
        crate::memory::types::AgentImprovementState {
            last_check: rec
                .last_check
                .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
                .or_else(|| {
                    doc.last_check
                        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                }),
            runs_since_check: rec.runs_since_check,
            no_op_streak: rec.no_op_streak,
            last_effect_verdict: rec.last_effect_verdict,
        }
    }

    /// 2a: count one completed task of `agent` toward the per-agent
    /// cooldown (called by the engine after every task, only while
    /// `auto_improve` is on).
    pub fn record_agent_task_completed(&self, agent: &str) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        doc.agents.entry(agent.to_string()).or_default().runs_since_check += 1;
        self.save_state_doc(&doc);
    }

    /// 2a: whether `agent`'s per-task cooldown has elapsed.
    ///
    /// Backoff: a no-op streak (consecutive checks that produced no
    /// suggestions) multiplies the base cooldown — streak 1: x1, 2: x2,
    /// 3: x3, 4+: x4 — so an agent whose lessons keep arriving but never
    /// yield a suggestion is re-checked less often; any productive check
    /// (or an applied one) resets the streak to 0.
    pub fn agent_improvement_due(&self, agent: &str, base_cooldown_tasks: usize) -> bool {
        let state = self.agent_improvement_state(agent);
        let base = base_cooldown_tasks.max(1) as u64;
        let mult = crate::agents::improvement::no_op_backoff_multiplier(state.no_op_streak);
        state.runs_since_check >= base * mult
    }

    /// 2a: record that an improvement check for `agent` just ran.
    /// `produced` = whether it yielded at least one suggestion (resets the
    /// no-op streak; an empty result extends it). Either way the per-agent
    /// cooldown counter restarts and the evidence gate baselines now.
    pub fn record_agent_improvement_check(&self, agent: &str, produced: bool) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        let rec = doc.agents.entry(agent.to_string()).or_default();
        // Milliseconds: lesson timestamps are sub-second (see AgentStateRec).
        rec.last_check = Some(chrono::Utc::now().timestamp_millis());
        rec.runs_since_check = 0;
        rec.no_op_streak = if produced { 0 } else { rec.no_op_streak.saturating_add(1) };
        self.save_state_doc(&doc);
    }

    /// 2a: persist the effect check's deterministic verdict for `agent`
    /// (written by `effect_check_section` after each check that has an
    /// applied-change marker to compare against).
    pub fn record_effect_verdict(&self, agent: &str, verdict: &str) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        doc.agents
            .entry(agent.to_string())
            .or_default()
            .last_effect_verdict = Some(verdict.to_string());
        self.save_state_doc(&doc);
    }

    /// 2a: per-agent evidence gate — whether NEW Lesson evidence exists
    /// since THIS agent's last check. Relevant evidence = lessons tagged
    /// `agent:<name>` plus agent-less (global) lessons; OTHER agents'
    /// lessons do not re-arm this agent.
    pub fn has_new_agent_improvement_evidence(&self, agent: &str) -> bool {
        let baseline = self.agent_improvement_state(agent).last_check;
        let agent_tag = format!("agent:{agent}");
        self.get_all_memories().iter().any(|e| {
            if e.r#type != MemoryType::Lesson {
                return false;
            }
            let relevant = e
                .tags
                .iter()
                .any(|t| t == &agent_tag)
                || !e.tags.iter().any(|t| t.starts_with("agent:"));
            if !relevant {
                return false;
            }
            match baseline {
                Some(ts) => e.timestamp.map(|t| t > ts).unwrap_or(false),
                None => true,
            }
        })
    }

    /// Load the improvement-check state document, tolerating a missing or
    /// corrupt file (both mean "default state"; a corrupt file is left in
    /// place for inspection — the next record overwrites it).
    fn load_state_doc(&self) -> ImprovementStateDoc {
        let path = self.improvement_state_path();
        if !path.exists() {
            return ImprovementStateDoc::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// 2a: atomic best-effort write of the state document (temp + rename,
    /// mirroring `save_memories`); failures are only logged — the state
    /// file must never be able to break task completion.
    fn save_state_doc(&self, doc: &ImprovementStateDoc) {
        let path = self.improvement_state_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::debug!("[MEMORY] Could not create state dir {:?}: {}", parent, e);
                return;
            }
        }
        let content = match serde_json::to_string(doc) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("[MEMORY] Could not serialize improvement state: {}", e);
                return;
            }
        };
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

    /// 1c/2a: the improvement-loop state snapshot for the
    /// `list_improvement_status` tool. Read-only and side-effect free — the
    /// persisted state (global + per-agent) plus the live evidence gate and
    /// config. Per-agent `last_check` here is the agent's OWN (no legacy
    /// fallback — the fallback is a baseline detail of the evidence gate,
    /// not something the status view should present).
    pub fn improvement_status(&self) -> ImprovementStatus {
        let config = self.config();
        let memories = self.get_all_memories();
        let doc = self.load_state_doc();
        ImprovementStatus {
            last_check: doc
                .last_check
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0)),
            has_new_evidence: self.has_new_improvement_evidence(),
            auto_improve: config.auto_improve,
            improvement_cooldown_tasks: config.improvement_cooldown_tasks,
            lesson_count: memories
                .iter()
                .filter(|e| e.r#type == MemoryType::Lesson)
                .count(),
            agents: doc
                .agents
                .iter()
                .map(|(name, rec)| {
                    (
                        name.clone(),
                        crate::memory::types::AgentImprovementState {
                            last_check: rec
                                .last_check
                                .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms)),
                            runs_since_check: rec.runs_since_check,
                            no_op_streak: rec.no_op_streak,
                            last_effect_verdict: rec.last_effect_verdict.clone(),
                        },
                    )
                })
                .collect(),
        }
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

/// 2a: persisted state of the auto-improvement loop (unix seconds).
///
/// v1 (I4) stored a single GLOBAL `last_check`. v2 keeps that field as a
/// LEGACY fallback baseline (an agent with no per-agent entry of its own
/// uses it as its evidence baseline) and adds per-agent counters, so a busy
/// agent can no longer starve an idle one's checks (or vice versa). Both
/// file shapes parse into this struct (`#[serde(default)]` everywhere), so
/// upgrading a v1 file is just reading it — the next write re-serializes it
/// as v2.
///
/// Private: only the `record_*` / `agent_improvement_*` manager methods
/// touch it.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
struct ImprovementStateDoc {
    #[serde(default)]
    version: u32,
    /// v1 global last-check (unix seconds) — legacy fallback baseline.
    #[serde(default)]
    last_check: Option<i64>,
    /// 2a: per-agent state, keyed by agent profile name.
    #[serde(default)]
    agents: std::collections::BTreeMap<String, AgentStateRec>,
}

/// On-disk per-agent record. `last_check` is unix MILLISECONDS (the lesson
/// timestamps carry sub-second precision — a seconds-precision baseline
/// would mark same-second lessons as "newer" than the check). Converted to
/// the public `AgentImprovementState` at the API boundary.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
struct AgentStateRec {
    #[serde(default)]
    last_check: Option<i64>,
    #[serde(default)]
    runs_since_check: u64,
    #[serde(default)]
    no_op_streak: u32,
    #[serde(default)]
    last_effect_verdict: Option<String>,
}

#[cfg(test)]
mod tests;
