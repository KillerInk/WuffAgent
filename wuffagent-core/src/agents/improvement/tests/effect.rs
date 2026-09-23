use super::*;

// ── I5: effect check (approved-change marker + outcomes since) ──

/// Build an approved-change marker (Fact, `improvement-applied` +
/// `agent:<name>`), backdated `days_ago` days.
fn marker_for(agent: &str, days_ago: i64) -> MemoryEntry {
    let mut e = MemoryEntry::new(
        MemoryType::Fact,
        &format!(
            "Prompt for agent '{}' changed via approved improvement.",
            agent
        ),
        "improvement-review",
        &["improvement-applied", &format!("agent:{}", agent)],
    );
    backdate(&mut e, days_ago);
    e
}

#[tokio::test]
async fn test_effect_check_includes_outcomes_since_marker() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    // Marker: approved prompt change 3 days ago.
    manager.add(marker_for("coder", 3)).unwrap();
    // Outcomes since the marker (tagged for the agent).
    let mut o1 = MemoryEntry::new(
        MemoryType::Lesson,
        "Verification failed: tests still red after the change",
        "verification",
        &["agent:coder", "verification"],
    );
    backdate(&mut o1, 1);
    manager.add(o1).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "User feedback: result rated poorly",
            "user-feedback",
            &["agent:coder", "user-feedback"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: r#"[{"agent_name": "coder", "prompt_change": "p", "rationale": "r"}]"#
            .to_string(),
        prompts: prompts.clone(),
    });
    let stats = RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 0,
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();

    assert_eq!(suggestions.len(), 1);
    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("was last changed via an approved improvement 3 day(s) ago"),
        "prompt: {}",
        prompt
    );
    assert!(
        prompt.contains("Outcomes recorded for this agent since that change:"),
        "prompt: {}",
        prompt
    );
    assert!(
        prompt.contains("tests still red after the change"),
        "prompt: {}",
        prompt
    );
    assert!(prompt.contains("result rated poorly"), "prompt: {}", prompt);
    assert!(
        prompt.contains("you may propose reverting the prompt"),
        "prompt: {}",
        prompt
    );
    // The effect-check input is attached as deterministic evidence.
    assert!(
        suggestions[0]
            .evidence
            .iter()
            .any(|e| e.starts_with("Effect check:")),
        "evidence: {:?}",
        suggestions[0].evidence
    );
}

#[tokio::test]
async fn test_effect_check_absent_without_marker() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A plain lesson",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let stats = RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 0,
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        !prompt.contains("was last changed via an approved improvement"),
        "no marker -> no effect-check section; prompt: {}",
        prompt
    );
    assert!(
        !prompt.contains("Outcomes recorded for this agent since that change:"),
        "prompt: {}",
        prompt
    );
}

#[tokio::test]
async fn test_effect_check_marker_without_outcomes_shows_none_yet() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    manager.add(marker_for("coder", 3)).unwrap();
    // A trigger lesson OLDER than the marker: it still triggers the check
    // (tag search has no timestamp filter) but is not an outcome "since".
    let mut old = MemoryEntry::new(
        MemoryType::Lesson,
        "A pre-change lesson",
        "agent",
        &["agent:coder"],
    );
    backdate(&mut old, 5);
    manager.add(old).unwrap();

    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let stats = RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 0,
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("Outcomes recorded for this agent since that change:"),
        "section must exist when a marker does; prompt: {}",
        prompt
    );
    assert!(prompt.contains("(none yet)"), "prompt: {}", prompt);
}

#[tokio::test]
async fn test_effect_check_excludes_outcomes_older_than_marker() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    manager.add(marker_for("coder", 3)).unwrap();
    // A trigger lesson and an outcome, both OLDER than the marker.
    let mut lesson = MemoryEntry::new(
        MemoryType::Lesson,
        "Trigger lesson from before the change",
        "agent",
        &["agent:coder"],
    );
    backdate(&mut lesson, 5);
    manager.add(lesson).unwrap();
    let mut old_outcome = MemoryEntry::new(
        MemoryType::Lesson,
        "Stale outcome from before the change",
        "verification",
        &["agent:coder", "verification"],
    );
    backdate(&mut old_outcome, 4);
    manager.add(old_outcome).unwrap();

    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let stats = RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 0,
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    // The section exists (marker present) but lists no outcomes — the stale
    // outcome is older than the marker (it still appears in the lessons
    // section, which has no timestamp filter).
    assert!(
        prompt.contains("Outcomes recorded for this agent since that change:"),
        "prompt: {}",
        prompt
    );
    assert!(prompt.contains("(none yet)"), "prompt: {}", prompt);
}
