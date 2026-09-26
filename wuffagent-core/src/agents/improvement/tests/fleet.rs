//! 2d: fleet-wide evidence + the fleet improver (2b(b)) — cross-agent
//! review of the whole fleet.
//!
//! NOTE: the metrics-dir and skills-dir test overrides are PROCESS-GLOBAL,
//! so these tests serialize on their own lock (same pattern as
//! `MetricsDirGuard` + skills/tests.rs).

use super::*;
use crate::agents::metrics::{MetricsLog, RunOutcome};
use crate::memory::skills::{set_skills_dir_for_testing, SkillStore};
use std::sync::Mutex;

/// 2d: no signal anywhere (no metrics, no lessons, no skills) → the evidence
/// block is empty (nothing for a fleet review to judge).
#[test]
fn test_fleet_evidence_empty_when_no_signal() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _metrics = MetricsDirGuard::new();

    let skills_dir = tempdir().unwrap();
    set_skills_dir_for_testing(Some(skills_dir.path().to_path_buf()));

    let (manager, _keep) = fresh_manager();
    let roster = vec![("coder".to_string(), "writes code".to_string())];
    assert_eq!(fleet_evidence_json(&manager, &roster, 7), "");
    set_skills_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(skills_dir.path());
}

/// 2d: per-agent metrics (mapped back to roster names), the agent's newest
/// tagged lessons, and the fleet skill usage (read vs. exists-but-never-read)
/// all land in the single JSON block.
#[test]
fn test_fleet_evidence_includes_metrics_lessons_and_skills() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let metrics = MetricsDirGuard::new();

    let skills_dir = tempdir().unwrap();
    set_skills_dir_for_testing(Some(skills_dir.path().to_path_buf()));
    let store = SkillStore::default();
    store.save("read-skill", "d", "w", "1. do").unwrap();
    store.save("stale-skill", "d", "w", "1. old").unwrap();

    let log = MetricsLog::new(metrics.dir());
    log.log_run("coder", 10, 3, 1, 5_000, RunOutcome::Verified, 100, 50);
    log.log_run("reviewer", 4, 0, 1, 2_000, RunOutcome::Verified, 60, 30);
    log.log_skill_use("read-skill");

    let (manager, _keep) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "coder lesson: verify builds before finishing",
            "test",
            &["agent:coder"],
        ))
        .unwrap();

    let roster = vec![
        ("coder".to_string(), "writes code".to_string()),
        ("reviewer".to_string(), "reviews code".to_string()),
    ];
    let json = fleet_evidence_json(&manager, &roster, 7);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["window_days"], 7);

    let agents = v["agents"].as_array().unwrap();
    let names: Vec<&str> = agents.iter().map(|a| a["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"coder"), "got: {names:?}");
    assert!(names.contains(&"reviewer"), "got: {names:?}");
    let coder = agents.iter().find(|a| a["name"] == "coder").unwrap();
    assert_eq!(coder["runs"], 1);
    assert_eq!(coder["tool_errors"], 3);
    assert_eq!(coder["top_lessons"].as_array().unwrap().len(), 1);
    assert!(coder["top_lessons"][0]
        .as_str()
        .unwrap()
        .contains("verify builds"));

    let read = v["skills_read"].as_array().unwrap();
    assert_eq!(read, &vec!["read-skill"]);
    let never = v["skills_never_read"].as_array().unwrap();
    assert!(never.iter().any(|n| n == "stale-skill"), "got: {never:?}");
    assert!(!never.iter().any(|n| n == "read-skill"), "got: {never:?}");

    set_skills_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(skills_dir.path());
}

/// 2d: the fleet prompt carries the roster, the focus line, and the evidence
/// block; the LLM's response is parsed into suggestions (evidence attached).
#[tokio::test]
async fn test_fleet_prompt_and_parse() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _metrics = MetricsDirGuard::new(); // empty metrics — skills+lessons carry the signal

    let skills_dir = tempdir().unwrap();
    set_skills_dir_for_testing(Some(skills_dir.path().to_path_buf()));
    SkillStore::default()
        .save("stale-skill", "d", "w", "1. old")
        .unwrap();

    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "a lesson so the fleet has evidence",
            "test",
            &["agent:coder"],
        ))
        .unwrap();

    let roster = vec![("coder".to_string(), "writes code".to_string())];
    let llm = Arc::new(CaptureLlm {
        response: r#"[{"agent_name":"coder","rationale":"shared failure pattern","prompt_change":"new prompt"}]"#
            .to_string(),
        prompts: prompts.clone(),
    });
    let suggestions =
        suggest_fleet_improvements(&manager, &roster, Some("reduce shell errors"), llm.as_ref())
            .await
            .unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].agent_name, "coder");
    assert!(!suggestions[0].evidence.is_empty(), "evidence must be attached");

    let prompt = &prompts.lock().unwrap()[0];
    assert!(prompt.contains("FLEET of AI agent profiles"), "prompt: {prompt}");
    assert!(prompt.contains("coder: writes code"), "prompt: {prompt}");
    assert!(
        prompt.contains("Concentrate the review on: reduce shell errors"),
        "prompt: {prompt}"
    );
    assert!(prompt.contains("Fleet evidence"), "prompt: {prompt}");
    assert!(
        prompt.contains("stale-skill"),
        "the never-read skill must be in the evidence: {prompt}"
    );
    set_skills_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(skills_dir.path());
}

/// 2d: no evidence in the window → the LLM is NOT called (cost gate), the
/// check just reports "nothing to improve".
#[tokio::test]
async fn test_fleet_no_evidence_skips_llm() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _metrics = MetricsDirGuard::new();
    let skills_dir = tempdir().unwrap();
    set_skills_dir_for_testing(Some(skills_dir.path().to_path_buf()));

    let (manager, _keep) = fresh_manager();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let roster = vec![("coder".to_string(), "d".to_string())];
    let suggestions = suggest_fleet_improvements(&manager, &roster, None, llm.as_ref())
        .await
        .unwrap();
    assert!(suggestions.is_empty());
    assert!(prompts.lock().unwrap().is_empty(), "no evidence → no LLM call");
    set_skills_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(skills_dir.path());
}

/// 2d: `auto_improve = false` → no check, even with evidence (the same
/// global gate as the per-agent path).
#[tokio::test]
async fn test_fleet_auto_improve_off_skips() {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _metrics = MetricsDirGuard::new();

    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        enabled: true,
        auto_improve: false,
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "evidence that would otherwise trigger a check",
            "test",
            &["agent:coder"],
        ))
        .unwrap();

    let prompts = Arc::new(Mutex::new(Vec::new()));
    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let roster = vec![("coder".to_string(), "d".to_string())];
    let suggestions = suggest_fleet_improvements(&manager, &roster, None, llm.as_ref())
        .await
        .unwrap();
    assert!(suggestions.is_empty());
    assert!(prompts.lock().unwrap().is_empty(), "auto_improve off → no LLM call");
}
