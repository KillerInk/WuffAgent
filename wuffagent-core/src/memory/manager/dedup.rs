//! Near-duplicate detection for memory inserts (extracted from manager.rs,
//! modularization phase 6).

use super::super::types::MemoryEntry;

/// Result of a memory insert attempt.
pub enum MemoryAddResult {
    /// The entry was new and got stored; carries the created entry (with its ID).
    Created(MemoryEntry),
    /// A strong near-duplicate already existed; `existing` is that entry.
    Duplicate(MemoryEntry),
}

/// Token overlap of two memory entries' content: (intersection, jaccard).
pub(crate) fn content_overlap(a: &MemoryEntry, b: &MemoryEntry) -> (usize, f64) {
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
pub(crate) fn is_near_duplicate(existing: &MemoryEntry, new: &MemoryEntry) -> bool {
    if existing.content == new.content {
        return true;
    }
    let (intersection, jaccard) = content_overlap(existing, new);
    jaccard >= 0.8 && intersection >= 3
}
