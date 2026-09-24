use super::*;

#[test]
fn test_add_and_search() {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();

    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "WuffAgent uses Cargo workspace",
            "test",
            &["project"],
        ))
        .unwrap();

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
        manager
            .add(MemoryEntry::new(
                MemoryType::Fact,
                &format!("Memory {}", i),
                "test",
                &[],
            ))
            .unwrap();
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
    assert_eq!(
        recent[0].id, new_id,
        "get_recent must skip superseded entries"
    );
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

    let e = MemoryEntry::new(
        MemoryType::Fact,
        "Original content here for testing purposes",
        "test",
        &["old"],
    );
    let id = e.id.clone();
    manager.add(e).unwrap();

    let updated = manager
        .update(
            &id,
            Some("Refined content here for testing purposes"),
            Some(vec!["new".to_string()]),
        )
        .unwrap();
    assert_eq!(updated.tags, vec!["new"]);
    assert_eq!(updated.supersedes, None);

    // Superseded entries are revived by update.
    let e2 = MemoryEntry::new(
        MemoryType::Fact,
        "Another original memory for the revival test",
        "test",
        &[],
    );
    let id2 = e2.id.clone();
    manager.add(e2).unwrap();
    manager.supersede(&id2, &id).unwrap();
    let revived = manager
        .update(
            &id2,
            Some("Revived memory content for the revival test now"),
            None,
        )
        .unwrap();
    assert_eq!(revived.supersedes, None);
    assert_eq!(
        revived.tags,
        Vec::<String>::new(),
        "tags unchanged when None is passed"
    );
    assert!(manager.get_recent(10).iter().any(|m| m.id == id2));
}

#[test]
fn test_update_without_content_keeps_existing_text() {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();

    let e = MemoryEntry::new(
        MemoryType::Fact,
        "Original content that must survive a tag-only update",
        "test",
        &["old"],
    );
    let id = e.id.clone();
    manager.add(e).unwrap();

    // Update with content = None: the existing content is kept, tags replaced.
    let updated = manager
        .update(&id, None, Some(vec!["new-tag".to_string(), "second".to_string()]))
        .unwrap();
    assert_eq!(updated.content, "Original content that must survive a tag-only update");
    assert_eq!(updated.tags, vec!["new-tag", "second"]);
    assert_eq!(updated.supersedes, None);

    // Same at the on-disk level: the persisted entry must be unchanged.
    let persisted = manager.find(&id).expect("entry still exists");
    assert_eq!(
        persisted.content, "Original content that must survive a tag-only update"
    );
    assert_eq!(persisted.tags, vec!["new-tag", "second"]);
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

    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "WuffAgent is a Rust project",
            "test",
            &["project"],
        ))
        .unwrap();

    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Shell tool plan was implemented with nested config",
            "test",
            &["shell"],
        ))
        .unwrap();

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

    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "The deploy script lives in scripts/deploy.sh",
            "test",
            &["deploy"],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "WuffAgent is a Rust project using eframe",
            "test",
            &["project"],
        ))
        .unwrap();

    // Query about the deploy script should surface the deploy entry first.
    let block = manager.build_context_block("run the deploy script");
    assert!(
        block.contains("scripts/deploy.sh"),
        "relevant entry must be injected: {block}"
    );
    assert!(
        !block.contains("eframe"),
        "unrelated entry must not fill the single slot: {block}"
    );
}

#[test]
fn test_query_injection_empty_store_fallback() {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        injection_max_entries: 5,
        injection_max_chars: 500,
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        injection_mode: crate::memory::types::InjectionMode::Always,
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
    assert!(
        block.contains("Recent unrelated architecture note"),
        "recent fallback expected: {block}"
    );

    // Smart mode with no hits: nothing is injected.
    manager.set_config(MemoryConfig {
        injection_mode: crate::memory::types::InjectionMode::Smart,
        ..manager.config().clone()
    });
    assert_eq!(
        manager.build_context_block("quantum entanglement theory"),
        ""
    );
}
