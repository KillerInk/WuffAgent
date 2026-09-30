use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::types::{MemoryConfig, MemoryEntry};

/// Minimum token length to be considered for matching (single characters are noise).
const MIN_TOKEN_LEN: usize = 2;

/// Common English + German stopwords that carry no retrieval signal.
const STOPWORDS: &[&str] = &[
    // English
    "a", "an", "the", "and", "or", "but", "if", "then", "else", "when", "while", "of", "in", "on",
    "at", "to", "from", "by", "for", "with", "about", "into", "over", "under", "is", "am", "are",
    "was", "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "will",
    "would", "should", "could", "can", "may", "might", "must", "it", "its", "this", "that",
    "these", "those", "i", "you", "he", "she", "we", "they", "them", "his", "her", "our", "your",
    "not", "no", "yes", "so", "as", "up", "down", "out", // German
    "und", "oder", "aber", "wenn", "dann", "dass", "dass", "bei", "aus", "auf", "den", "dem", "der",
    "des", "die", "ein", "eine", "einen", "einem", "einer", "einem", "mit", "nach", "von", "vor",
    "zu", "zum", "zur", "ist", "sind", "war", "waren", "hat", "hatte", "haben", "habe", "wird",
    "wurde", "ich", "du", "er", "sie", "wir", "ihr", "man", "nicht", "ja", "nein", "auch", "als",
    "am", "im", "ins", "um", "noch", "nur", "sehr", "wie", "was", "wer", "wo", "wohin", "wieso",
    "worum", "wollte", "kann", "muss",
];

/// The stopwords as a set for O(1) membership tests, built once on first
/// use. `tokenize` filters every token through this, so the old linear
/// `contains` over ~100 words was a per-token cost paid for every query and
/// every memory entry in each search.
fn stopword_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

fn is_stopword(token: &str) -> bool {
    stopword_set().contains(token)
}

/// Tokenize text into words for keyword matching.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|s| s.len() >= MIN_TOKEN_LEN && !is_stopword(s))
        .map(|s| s.to_string())
        .collect()
}

/// Score a memory entry against an already-prepared query (the lowercased
/// query text + its tokens). This is the per-entry hot path: `keyword_search`
/// prepares the query ONCE and scores every entry against the same values, so
/// nothing query-derived is recomputed per entry (the old code re-ran
/// `tokenize(query)` and `query.to_lowercase()` inside the scorer for every
/// entry in the store).
fn keyword_score_prepared(
    entry: &MemoryEntry,
    query_lower: &str,
    query_tokens: &[String],
) -> f64 {
    let entry_text = format!("{} {}", entry.content, entry.tags.join(" "));
    let entry_tokens = tokenize(&entry_text);

    if entry_tokens.is_empty() || query_tokens.is_empty() {
        return 0.0;
    }

    let entry_freq: HashMap<&str, usize> =
        entry_tokens
            .iter()
            .map(|t| t.as_str())
            .fold(HashMap::new(), |mut map, t| {
                *map.entry(t).or_insert(0) += 1;
                map
            });

    let total_tokens = entry_tokens.len() as f64;
    let mut score = 0.0;

    for q_token in query_tokens {
        if let Some(&count) = entry_freq.get(q_token.as_str()) {
            // Term frequency weighted by entry confidence.
            score += (count as f64 / total_tokens) * entry.confidence as f64;
        }
    }

    // Bonus for tag matches: exact tag match on a query token, or the tag is a
    // prefix of a query token (so tag "rust" matches query "rustc").
    for tag in &entry.tags {
        let tag_lower = tag.to_lowercase();
        let is_exact = query_tokens.iter().any(|t| *t == tag_lower);
        let is_prefix = !tag_lower.is_empty()
            && query_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|t| t.len() > tag_lower.len() && t.starts_with(&tag_lower));
        if is_exact || is_prefix {
            score += 0.5;
        }
    }

    score
}

/// Search for relevant memories using keyword matching.
/// Returns entries sorted by relevance score (descending), capped at max_results.
pub fn keyword_search<'a>(
    entries: &'a [MemoryEntry],
    query: &str,
    max_results: usize,
) -> Vec<&'a MemoryEntry> {
    if query.trim().is_empty() {
        return Vec::new();
    }

    // Prepare the query ONCE, not per entry: the scorer used to be reached as
    // `keyword_score(e, query)`, which re-tokenized and re-lowercased the
    // (short, identical) query for every entry — O(entries) redundant work
    // per search.
    let query_lower = query.to_lowercase();
    let query_tokens = tokenize(query);

    let mut scored: Vec<(&MemoryEntry, f64)> = entries
        .iter()
        .filter(|e| !e.is_expired() && e.supersedes.is_none())
        .map(|e| (e, keyword_score_prepared(e, &query_lower, &query_tokens)))
        .filter(|(_, score)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    scored
        .into_iter()
        .take(max_results)
        .map(|(e, _)| e)
        .collect()
}

/// Get the N most recent memories (fallback when no query).
pub fn get_recent_memories(entries: &[MemoryEntry], count: usize) -> Vec<&MemoryEntry> {
    let mut active: Vec<&MemoryEntry> = entries
        .iter()
        .filter(|e| !e.is_expired() && e.supersedes.is_none())
        .collect();

    active.sort_by(|a, b| {
        let ta = a.timestamp.unwrap_or_default();
        let tb = b.timestamp.unwrap_or_default();
        tb.cmp(&ta)
    });

    active.into_iter().take(count).collect()
}

/// Search memories based on config settings.
pub fn search_memories<'a>(
    entries: &'a [MemoryEntry],
    query: &str,
    config: &MemoryConfig,
) -> Vec<&'a MemoryEntry> {
    keyword_search(entries, query, config.injection_max_entries)
}

#[cfg(test)]
mod tests;
