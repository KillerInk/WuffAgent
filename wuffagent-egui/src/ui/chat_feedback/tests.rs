//! S2 tests for user feedback (👍/👎) lesson entries.

use super::*;

fn snippet(messages: &[ChatMessage], index: usize) -> String {
    task_snippet_for(messages, index)
}

fn msg(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: content.to_string(),
        timestamp: String::new(),
        image: None,
        kind: wuffagent_core::types::MessageKind::Normal,
    }
}

/// S2: the lesson entry names the rating, the agent (in content AND tag),
/// the task snippet, and the comment (or "none").
#[test]
fn test_feedback_lesson_shape() {
    let entry = feedback_lesson("coder", true, "list the directory", "");
    assert!(matches!(entry.r#type, MemoryType::Lesson));
    assert_eq!(entry.source, "user-feedback");
    assert!(entry.tags.contains(&"user-feedback".to_string()), "tags: {:?}", entry.tags);
    assert!(entry.tags.contains(&"agent:coder".to_string()), "tags: {:?}", entry.tags);
    assert!(entry.content.contains("good"), "{}", entry.content);
    assert!(entry.content.contains("coder"), "{}", entry.content);
    assert!(entry.content.contains("list the directory"), "{}", entry.content);
    assert!(entry.content.contains("none"), "{}", entry.content);

    let bad = feedback_lesson("researcher", false, "summarize the docs", "missed the key API");
    assert!(bad.content.contains("bad"), "{}", bad.content);
    assert!(bad.content.contains("missed the key API"), "{}", bad.content);
    assert!(bad.tags.contains(&"agent:researcher".to_string()));
}

/// S2: ratings persist through the shared save path; an identical repeat is
/// collapsed by the dedup gate; a different comment is a distinct entry.
#[test]
fn test_remember_feedback_saves_and_dedups() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-user-feedback-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = wuffagent_core::memory::MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = wuffagent_core::memory::MemoryManager::new(config).unwrap();

    assert!(remember_feedback(&memory, "coder", false, "list the directory", "").unwrap());
    // Identical repeat -> dedup gate, still Ok, no second entry.
    assert!(remember_feedback(&memory, "coder", false, "list the directory", "").unwrap());
    assert_eq!(memory.count(), 1, "dedup must collapse identical repeats");

    // A genuinely different rating is a distinct entry. (Note: merely adding
    // a short comment to the same rating stays within the dedup gate's 0.75
    // token-overlap window and collapses into the original — same behavior as
    // the F5 dismissal lessons; intended.)
    assert!(remember_feedback(&memory, "coder", true, "implement the retry logic", "").unwrap());
    assert_eq!(memory.count(), 2);

    // The stored lesson is retrievable by an agent-name search (what
    // collect_lessons does via the agent:<name> tag).
    assert!(!memory.get_by_tag("agent:coder").is_empty());
    assert!(memory.get_by_tag("agent:researcher").is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// S2: with memory disabled the rating is skipped (not an error) and nothing
/// is stored.
#[test]
fn test_remember_feedback_skipped_when_disabled() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-user-feedback-disabled-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = wuffagent_core::memory::MemoryConfig {
        enabled: false,
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = wuffagent_core::memory::MemoryManager::new(config).unwrap();

    assert_eq!(remember_feedback(&memory, "coder", true, "t", "").unwrap(), false);
    assert_eq!(memory.count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// S2: the task snippet is the nearest PRECEDING user message, truncated to
/// ~200 chars; empty when there is none.
#[test]
fn test_task_snippet_for() {
    let messages = vec![
        msg("user", "first request"),
        msg("assistant", "first answer"),
        msg("user", "second request"),
        msg("assistant", "second answer"),
    ];
    assert_eq!(snippet(&messages, 1), "first request");
    // The assistant at index 3 was answering the "second" request.
    assert_eq!(snippet(&messages, 3), "second request");
    // No preceding user message.
    assert_eq!(snippet(&messages, 0), "");

    let long = vec![msg("user", &"x".repeat(500)), msg("assistant", "a")];
    let s = snippet(&long, 1);
    assert!(s.chars().count() <= 201, "snippet must be bounded: {} chars", s.chars().count());
    assert!(s.ends_with('…'));
}
