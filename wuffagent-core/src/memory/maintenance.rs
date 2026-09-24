//! LLM memory-maintenance pass over the store (batched merge/update/delete
//! actions) plus its report types. Sits next to super::manager: the
//! impl MemoryManager blocks in this file are pure code motion from there.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing;

use crate::types::Message;

use super::manager::{MemoryAddResult, MemoryManager};
use super::types::{MemoryEntry, MemoryType};

impl MemoryManager {
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
        entries.sort_by_key(|e| {
            e.timestamp
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
        });
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
            format!(
                "Maintenance complete: {} ({} batches)",
                parts.join(", "),
                total_batches
            )
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
            tracing::warn!(
                "[MEMORY] {failures} maintenance batch(es) failed; see earlier log lines"
            );
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
        entries.sort_by_key(|e| {
            e.timestamp
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
        });
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
                .map(|ts| {
                    format!(
                        "{}d old",
                        chrono::Utc::now()
                            .signed_duration_since(ts)
                            .num_days()
                            .max(0)
                    )
                })
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
        let per_step =
            std::time::Duration::from_secs(self.config().memory_maintenance_timeout_secs.max(10));
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
            Ok(MaintenanceReport::summary(
                "Maintenance complete: no changes suggested",
            ))
        } else {
            tracing::info!(
                "[MEMORY] Applying maintenance: {} merges, {} updates, {} deletes",
                actions.merge.len(),
                actions.update.len(),
                actions.delete.len()
            );
            Ok(apply_maintenance_actions(
                self,
                chunk,
                total_entries,
                &actions,
            ))
        }
    }
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
            skipped.push(format!(
                "merge needs at least 2 known ids (got {})",
                merge.ids.len()
            ));
            continue;
        }
        // Safety: never let a single merge consume the entire store
        // (checked against the global store size, not this batch).
        if total_entries > 0 && valid.len() >= total_entries {
            skipped.push(format!(
                "merge of {} entries would remove the whole store",
                valid.len()
            ));
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
        if let Err(e) = manager.update(&update.id, Some(update.content.trim()), tags) {
            skipped.push(format!("update of {} failed: {}", update.id, e));
        }
    }

    // Delete: drop stale entries, but never the last remaining entry.
    let mut current_count = manager.count();
    for delete in actions
        .delete
        .iter()
        .filter(|d| existing_ids.contains(d.id.as_str()))
    {
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

fn tags_from_sources(
    entries: &[MemoryEntry],
    ids: &[&String],
) -> std::collections::VecDeque<String> {
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
