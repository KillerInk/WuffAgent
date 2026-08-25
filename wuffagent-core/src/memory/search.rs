use std::collections::HashMap;
use tracing;

use super::types::{MemoryEntry, MemoryConfig};

/// Tokenize text into words for keyword matching.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

/// Score a memory entry against a query using keyword matching.
fn keyword_score(entry: &MemoryEntry, query: &str) -> f64 {
    let query_tokens = tokenize(query);
    let entry_text = format!("{} {}", entry.content, entry.tags.join(" "));
    let entry_tokens = tokenize(&entry_text);

    if entry_tokens.is_empty() || query_tokens.is_empty() {
        return 0.0;
    }

    let entry_freq: HashMap<&str, usize> = entry_tokens.iter()
        .map(|t| t.as_str())
        .fold(HashMap::new(), |mut map, t| {
            *map.entry(t).or_insert(0) += 1;
            map
        });

    let total_tokens = entry_tokens.len() as f64;
    let mut score = 0.0;

    for q_token in &query_tokens {
        if let Some(&count) = entry_freq.get(q_token.as_str()) {
            // TF-IDF style: term frequency * log(N/document_freq)
            // Simplified: just use term frequency weighted by entry confidence
            score += (count as f64 / total_tokens) * entry.confidence as f64;
        }
    }

    // Bonus for tag matches
    for tag in &entry.tags {
        if query.to_lowercase().contains(&tag.to_lowercase()) {
            score += 0.5;
        }
    }

    score
}

/// Search for relevant memories using keyword matching.
/// Returns entries sorted by relevance score (descending), capped at max_results.
pub fn keyword_search<'a>(entries: &'a [MemoryEntry], query: &str, max_results: usize) -> Vec<&'a MemoryEntry> {
    if query.trim().is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(&MemoryEntry, f64)> = entries.iter()
        .filter(|e| !e.is_expired() && e.supersedes.is_none())
        .map(|e| (e, keyword_score(e, query)))
        .filter(|(_, score)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    scored.into_iter()
        .take(max_results)
        .map(|(e, _)| e)
        .collect()
}

/// Get the N most recent memories (fallback when no query).
pub fn get_recent_memories(entries: &[MemoryEntry], count: usize) -> Vec<&MemoryEntry> {
    let mut active: Vec<&MemoryEntry> = entries.iter()
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
pub fn search_memories<'a>(entries: &'a [MemoryEntry], query: &str, config: &MemoryConfig) -> Vec<&'a MemoryEntry> {
    let max_results = config.injection_max_entries;

    match config.search_mode {
        super::types::SearchMode::Keyword => {
            keyword_search(entries, query, max_results)
        }
        super::types::SearchMode::Llm => {
            // LLM search will be implemented in Phase 4
            // For now, fall back to keyword search
            tracing::debug!("LLM search requested, falling back to keyword search");
            keyword_search(entries, query, max_results)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Hello world! This is a test.");
        assert_eq!(tokens, vec!["hello", "world", "this", "is", "a", "test"]);
    }

    #[test]
    fn test_keyword_score() {
        let entry = MemoryEntry::new(
            super::super::types::MemoryType::Fact,
            "WuffAgent uses Cargo workspace with wuffagent-core and wuffagent-egui",
            "test",
            &["project", "architecture"],
        );

        let score = keyword_score(&entry, "wuffagent cargo workspace");
        assert!(score > 0.0);

        let score2 = keyword_score(&entry, "completely unrelated topic");
        assert!(score2 < score);
    }

    #[test]
    fn test_keyword_search() {
        let entries = vec![
            MemoryEntry::new(
                super::super::types::MemoryType::Fact,
                "WuffAgent is a Rust project",
                "test",
                &["project"],
            ),
            MemoryEntry::new(
                super::super::types::MemoryType::Lesson,
                "Shell tool plan was implemented with nested config",
                "test",
                &["shell", "tools"],
            ),
        ];

        let results = keyword_search(&entries, "shell tool", 2);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Shell tool plan was implemented with nested config");
    }

    #[test]
    fn test_get_recent_memories() {
        let e1 = MemoryEntry::new(super::super::types::MemoryType::Fact, "First", "test", &[]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let e2 = MemoryEntry::new(super::super::types::MemoryType::Fact, "Second", "test", &[]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let e3 = MemoryEntry::new(super::super::types::MemoryType::Fact, "Third", "test", &[]);
        let entries = vec![e1, e2, e3];

        let recent = get_recent_memories(&entries, 2);
        assert_eq!(recent.len(), 2);
        // Should return the two most recent: Third (newest) then Second
        assert_eq!(recent[0].content, "Third");
        assert_eq!(recent[1].content, "Second");
    }
}
