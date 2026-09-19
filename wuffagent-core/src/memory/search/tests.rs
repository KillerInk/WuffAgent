//! Unit tests for the `search` module (see `super`).

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
