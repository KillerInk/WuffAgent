use super::*;

/// S1: a non-first-try outcome is stored as a `lesson` tagged
/// `agent:<name>` + `verification`, and a repeated identical outcome
/// collapses via the dedup gate.
#[test]
fn test_record_verification_outcome_stores_and_dedups() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-verification-outcome-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::memory::MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = crate::memory::MemoryManager::new(config).unwrap();

    let saved = record_verification_outcome(
        &memory,
        "coder",
        "gave_up",
        2,
        "NEEDS_FIX: the response misses b.txt",
        "list the directory",
    )
    .unwrap();
    assert!(saved, "entry stored");
    // Identical repeat: dedup gate collapses it.
    record_verification_outcome(
        &memory,
        "coder",
        "gave_up",
        2,
        "NEEDS_FIX: the response misses b.txt",
        "list the directory",
    )
    .unwrap();
    assert_eq!(memory.count(), 1, "identical repeats must collapse");

    let stored = memory.get_all_memories();
    assert_eq!(stored.len(), 1);
    let e = &stored[0];
    assert!(matches!(e.r#type, MemoryType::Lesson));
    assert_eq!(e.source, "verification");
    assert!(
        e.tags.contains(&"agent:coder".to_string()),
        "tags: {:?}",
        e.tags
    );
    assert!(
        e.tags.contains(&"verification".to_string()),
        "tags: {:?}",
        e.tags
    );
    // Names the agent, the verdict, the judge reason, and the task — so
    // collect_lessons' agent-name+task keyword search finds it.
    assert!(e.content.contains("coder"), "{}", e.content);
    assert!(e.content.contains("gave_up"), "{}", e.content);
    assert!(
        e.content.contains("NEEDS_FIX: the response misses b.txt"),
        "{}",
        e.content
    );
    assert!(e.content.contains("list the directory"), "{}", e.content);

    let _ = std::fs::remove_dir_all(&dir);
}

/// S1: outcome storage is skipped (not an error) when memory is disabled.
#[test]
fn test_record_verification_outcome_skipped_when_disabled() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-verification-outcome-disabled-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::memory::MemoryConfig {
        enabled: false,
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = crate::memory::MemoryManager::new(config).unwrap();

    let saved = record_verification_outcome(&memory, "coder", "gave_up", 2, "r", "t").unwrap();
    assert!(!saved, "disabled memory -> skipped");
    assert_eq!(memory.count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// S1: long judge reasons and task snippets are truncated in the stored
/// content (bounded entry size), and an empty reason degrades gracefully.
#[test]
fn test_record_verification_outcome_truncates() {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent-verification-outcome-trunc-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::memory::MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = crate::memory::MemoryManager::new(config).unwrap();

    record_verification_outcome(
        &memory,
        "coder",
        "verified_after_retry",
        2,
        &"x".repeat(500),
        &"t".repeat(400),
    )
    .unwrap();
    let e = &memory.get_all_memories()[0];
    // Fixed prefix + 300-char reason + 200-char task, each plus an ellipsis.
    assert!(
        e.content.len() < 900,
        "content must be bounded: {} chars",
        e.content.len()
    );
    assert!(e.content.contains('…'), "truncation marker expected");
    assert!(e.content.contains("verified_after_retry"), "{}", e.content);

    // Empty reason -> placeholder instead of a dangling "Judge: ."
    record_verification_outcome(&memory, "coder", "gave_up", 2, "  ", "short task").unwrap();
    let entries = memory.get_all_memories();
    assert!(
        entries
            .iter()
            .any(|e| e.content.contains("(no reason given)")),
        "entries: {:?}",
        entries
            .iter()
            .map(|e| e.content.as_str())
            .collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
