use std::collections::HashMap;

use super::types::{MemoryEntry, MemoryConfig};

/// Minimum token length to be considered for matching (single characters are noise).
const MIN_TOKEN_LEN: usize = 2;

/// Common English + German stopwords that carry no retrieval signal.
const STOPWORDS: &[&str] = &[
    // English
    "a", "an", "the", "and", "or", "but", "if", "then", "else", "when", "while",
    "of", "in", "on", "at", "to", "from", "by", "for", "with", "about", "into",
    "over", "under", "is", "am", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "should",
    "could", "can", "may", "might", "must", "it", "its", "this", "that",
    "these", "those", "i", "you", "he", "she", "we", "they", "them", "his",
    "her", "our", "your", "not", "no", "yes", "so", "as", "up", "down", "out",
    // German
    "und", "oder", "aber", "wenn", "dann", "dass", "dass", "bei", "aus",
    "auf", "den", "dem", "der", "des", "die", "ein", "eine", "einen", "einem",
    "einer", "einem", "mit", "nach", "von", "vor", "zu", "zum", "zur", "ist",
    "sind", "war", "waren", "hat", "hatte", "haben", "habe", "wird", "wurde",
    "ich", "du", "er", "sie", "wir", "ihr", "man", "nicht", "ja", "nein",
    "auch", "als", "am", "im", "ins", "um", "noch", "nur", "sehr", "wie",
    "was", "wer", "wo", "wohin", "wieso", "worum", "wollte", "kann", "muss",
];

fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
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
            // Term frequency weighted by entry confidence.
            score += (count as f64 / total_tokens) * entry.confidence as f64;
        }
    }

    // Bonus for tag matches: exact tag match on a query token, or the tag is a
    // prefix of a query token (so tag "rust" matches query "rustc").
    let query_lower = query.to_lowercase();
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
    keyword_search(entries, query, config.injection_max_entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Hello world! This is a test.");
        assert_eq!(tokens, vec!["hello", "world", "test"]);
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

    #[test]
    fn test_tokenize_filters_stopwords_and_short_tokens() {
        let tokens = tokenize("The agent and rustc 7 are working");
        assert!(tokens.contains(&"agent".to_string()));
        assert!(tokens.contains(&"rustc".to_string()));
        // Stopwords and single characters are filtered.
        assert!(!tokens.contains(&"the".to_string()));
        assert!(!tokens.contains(&"and".to_string()));
        assert!(!tokens.contains(&"7".to_string()));
    }

    #[test]
    fn test_stopword_only_query_scores_zero() {
        let entry = MemoryEntry::new(
            super::super::types::MemoryType::Fact,
            "cargo workspace layout",
            "test",
            &[],
        );
        // "the of and" has no signal tokens left after stopword filtering.
        assert_eq!(keyword_score(&entry, "the of and"), 0.0);
    }

    #[test]
    fn test_tag_prefix_match() {
        let entry = MemoryEntry::new(
            super::super::types::MemoryType::Fact,
            "unrelated content here",
            "test",
            &["rust"],
        );
        // Query contains a word with the tag as a prefix ("rustc") -> tag bonus.
        assert!(keyword_score(&entry, "how does rustc work") > 0.0);
        // A word containing the tag but not as a prefix should not match the tag.
        let entry2 = MemoryEntry::new(
            super::super::types::MemoryType::Fact,
            "unrelated content here",
            "test",
            &["rust"],
        );
        assert_eq!(keyword_score(&entry2, "a trust fund plan"), 0.0);
    }
}
