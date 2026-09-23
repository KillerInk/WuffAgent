use super::*;

#[test]
fn test_collect_lessons_prefers_task_relevant_search() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Use cargo check before running tests in the rust workspace",
            "agent",
            &[],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "An unrelated fact about rust crates and packaging",
            "agent",
            &[],
        ))
        .unwrap();

    let lessons = collect_lessons(&manager, "coder", "run the tests in the rust workspace");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("cargo check"));
}

#[test]
fn test_collect_lessons_falls_back_to_recent_when_no_match() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "The shell tool fails when the working directory does not exist",
            "agent",
            &["shell"],
        ))
        .unwrap();

    // No keyword overlap between the query and the stored lesson, so the
    // recent-lessons fallback must surface it instead of skipping the check.
    let lessons = collect_lessons(&manager, "coder", "completely unrelated topic zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("shell tool"));
}

#[test]
fn test_collect_lessons_empty_store() {
    let (manager, _dir) = fresh_manager();
    assert!(collect_lessons(&manager, "coder", "some task").is_empty());
}

/// S3: a lesson tagged `agent:<name>` is collected for that agent even when
/// the task text has no keyword overlap with it (the tag is the primary
/// per-agent signal).
#[test]
fn test_collect_lessons_tag_first() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Prefer streaming responses for large files to avoid timeouts",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    // No overlap between the tag-less lesson text and this task — only the
    // agent tag can surface it.
    let lessons = collect_lessons(&manager, "coder", "topic with no keyword overlap zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("streaming responses"));
}

/// S3: the tag path WINS over the text-search path — a tagged lesson for the
/// agent is returned and an untagged lesson that matches the task text is not
/// mixed in while the tag path has hits.
#[test]
fn test_collect_lessons_tag_wins_over_search() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Tagged lesson about workspace layout for the coder agent",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Untagged lesson mentioning workspace layout keywords",
            "agent",
            &[],
        ))
        .unwrap();

    // The task text strongly matches the UNTAGGED lesson, but the tagged one
    // must win (and be the only result).
    let lessons = collect_lessons(&manager, "coder", "workspace layout keywords");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("Tagged lesson"));
}

/// S3: tag filtering is per agent — a lesson tagged for the researcher is
/// not picked up by the coder's tag path, and the researcher's own
/// collect_lessons finds it via the tag alone (no task-text overlap needed).
#[test]
fn test_collect_lessons_tag_is_per_agent() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Researcher workflow note on citation style",
            "agent",
            &["agent:researcher"],
        ))
        .unwrap();

    // The manager-level tag filter is exact per agent.
    assert_eq!(manager.get_by_tag("agent:researcher").len(), 1);
    assert!(manager.get_by_tag("agent:coder").is_empty());

    let lessons = collect_lessons(&manager, "researcher", "unrelated topic zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("citation style"));
}
