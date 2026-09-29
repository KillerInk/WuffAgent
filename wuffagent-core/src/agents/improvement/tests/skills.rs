use super::*;
use crate::memory::skills::{set_skills_dir_for_testing, SkillMeta, SkillStore};
use std::sync::{Arc, Mutex};

// ── 3b: the skill retire-signal (never-read skills → delete/merge suggestions) ──

/// Pure: the retire line lists the skills that exist but were never read in
/// the window; every name on the "read" list is excluded.
#[test]
fn test_skill_retire_line_lists_only_unused_skills() {
    let all = vec![
        SkillMeta {
            name: "used-skill".to_string(),
            description: "d".to_string(),
            when_to_use: "w".to_string(),
        },
        SkillMeta {
            name: "rotted-skill".to_string(),
            description: "d".to_string(),
            when_to_use: "w".to_string(),
        },
    ];
    let used = vec!["used-skill".to_string()];
    let line = skill_retire_line(&all, &used, 7);
    assert!(
        line.contains("NEVER read in the last 7 day(s): rotted-skill"),
        "got: {line}"
    );
    assert!(
        !line.contains("used-skill"),
        "a read skill must not be on the retire line: {line}"
    );
    assert!(line.contains("delete"), "got: {line}");
}

/// Pure: no skills, or all skills used in the window, → empty (no signal).
#[test]
fn test_skill_retire_line_empty_when_nothing_unused() {
    assert_eq!(skill_retire_line(&[], &[], 7), "");
    let all = vec![SkillMeta {
        name: "used".to_string(),
        description: "d".to_string(),
        when_to_use: "w".to_string(),
    }];
    assert_eq!(
        skill_retire_line(&all, &["used".to_string()], 7),
        "",
        "every existing skill was read → no retire signal"
    );
}

/// Pure: the name list is capped at 10 with a "+N more" tail.
#[test]
fn test_skill_retire_line_caps_at_ten() {
    let all: Vec<SkillMeta> = (0..12)
        .map(|i| SkillMeta {
            name: format!("skill-{i:02}"),
            description: "d".to_string(),
            when_to_use: "w".to_string(),
        })
        .collect();
    let line = skill_retire_line(&all, &[], 7);
    assert!(line.contains("skill-00"), "got: {line}");
    assert!(line.contains("skill-09"), "got: {line}");
    assert!(!line.contains("skill-10"), "past the cap: {line}");
    assert!(line.contains(" (+2 more)"), "got: {line}");
}

/// E2E: an existing but never-read skill lands on a "NEVER read" line in the
/// extraction prompt (and a read one does not). The global skills-dir
/// override is process-wide, so this test serializes on a lock and restores
/// the override afterwards.
#[tokio::test]
async fn test_prompt_includes_retire_line_for_never_read_skills() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _metrics = MetricsDirGuard::new(); // empty metrics dir → no skill usage

    let skills_dir = tempdir().unwrap();
    set_skills_dir_for_testing(Some(skills_dir.path().to_path_buf()));
    let store = SkillStore::default();
    store.save("fresh-skill", "a procedure", "when needed", "1. do it").unwrap();
    store.save("stale-skill", "an old procedure", "rarely", "1. do the old thing").unwrap();

    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    // Trigger the check: the manager starts with no lessons (threshold = 1).
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson so the improvement check runs",
            "test",
            &["agent:coder"],
        ))
        .unwrap();
    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let stats = crate::agents::RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 0,
        ..Default::default()
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "do the thing",
        "done",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    // Both skills are unused in the window → both on the retire line.
    assert!(
        prompt.contains("NEVER read in the last 7 day(s): fresh-skill, stale-skill"),
        "prompt: {prompt}"
    );
    // ...and the maintenance instruction is present.
    assert!(
        prompt.contains("Skill maintenance:"),
        "prompt: {prompt}"
    );
    set_skills_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(skills_dir.path());
}
