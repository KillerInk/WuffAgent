use super::*;

// ── I4: cost control (evidence gate + state persistence) ──


#[test]
fn test_evidence_gate_no_state_and_no_lessons() {
    let (manager, _dir) = fresh_manager();
    assert!(
        !manager.has_new_improvement_evidence(),
        "no state file + no lessons -> no evidence"
    );
}

#[test]
fn test_evidence_gate_no_state_with_lessons() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson that counts as evidence",
            "agent",
            &[],
        ))
        .unwrap();
    assert!(
        manager.has_new_improvement_evidence(),
        "no state file + a lesson -> evidence exists (first check baselines it)"
    );
}

#[test]
fn test_evidence_gate_ignores_non_lesson_entries() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "A fact is not evidence",
            "agent",
            &[],
        ))
        .unwrap();
    assert!(
        !manager.has_new_improvement_evidence(),
        "Facts (e.g. I5 markers) are reference points, not evidence"
    );
}

#[test]
fn test_evidence_gate_old_lessons_then_new_lesson() {
    let (manager, _dir) = fresh_manager();
    // A lesson that predates the check.
    let mut old = MemoryEntry::new(MemoryType::Lesson, "An old lesson", "agent", &[]);
    backdate(&mut old, 1);
    manager.add(old).unwrap();

    manager.record_improvement_check();
    assert!(
        !manager.has_new_improvement_evidence(),
        "only pre-check lessons -> no NEW evidence"
    );

    // A new lesson after the check re-arms the gate.
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A brand new lesson after the check",
            "agent",
            &[],
        ))
        .unwrap();
    assert!(
        manager.has_new_improvement_evidence(),
        "post-check lesson -> evidence again"
    );
}

#[test]
fn test_improvement_state_persists_across_restart() {
    let dir = tempdir().unwrap();
    let make = || {
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        MemoryManager::new(config).unwrap()
    };

    let first = make();
    first
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson before restart",
            "agent",
            &[],
        ))
        .unwrap();
    first.record_improvement_check();
    drop(first);

    // "Restart": a fresh manager on the same dir must see the recorded check,
    // so the pre-restart lesson is not re-counted as new evidence.
    let second = make();
    assert!(
        !second.has_new_improvement_evidence(),
        "state file must survive a restart"
    );
}

#[test]
fn test_improvement_state_corrupt_file_tolerated() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("improvement_state.json"), "not json at all").unwrap();

    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson",
            "agent",
            &[],
        ))
        .unwrap();

    // Corrupt state -> treated as "no check recorded" -> lessons are evidence.
    assert!(manager.has_new_improvement_evidence());

    // Recording overwrites the corrupt file with valid JSON.
    manager.record_improvement_check();
    let content = std::fs::read_to_string(dir.path().join("improvement_state.json")).unwrap();
    assert!(content.contains("last_check"), "content: {}", content);
    assert!(
        serde_json::from_str::<serde_json::Value>(&content).is_ok(),
        "state file must be valid JSON after recording"
    );
}
