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
    let _guard = MetricsDirGuard::new();
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    use std::io::Write;
    // Marker is 3 days ago → after window [3,0]; give it 3 runs so the
    // JUDGEABLE path renders (the "you may propose reverting" sentence is
    // part of that path, not the deferred one-liner).
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    writeln!(f, "{}", run_line(1, 4, 1)).unwrap();
    writeln!(f, "{}", run_line(2, 4, 1)).unwrap();
    writeln!(f, "{}", run_line(0, 4, 1)).unwrap();
    drop(f);

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

/// One JSON metrics `run` line backdated `days_ago` days (written directly —
/// `MetricsLine`'s variant fields are not constructible outside the metrics
/// module).
fn run_line(days_ago: i64, calls: u32, errors: u32) -> String {
    serde_json::json!({
        "kind": "run",
        "ts": (chrono::Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339(),
        "tool_calls": calls,
        "tool_errors": errors,
        "verification_attempts": 1,
        "duration_ms": 1_000,
        "outcome": "gave_up",
    })
    .to_string()
}

/// 1a: the effect check shows before/after per-agent METRICS windows (not
/// just the lesson list), and the evidence line carries the run counts.
#[tokio::test]
async fn test_effect_check_before_after_metrics() {
    let _guard = MetricsDirGuard::new();
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    // Marker is 3 days ago → BEFORE window = [6, 3] days ago, AFTER = [3, 0].
    writeln!(f, "{}", run_line(4, 5, 1)).unwrap();
    writeln!(f, "{}", run_line(5, 5, 1)).unwrap(); // before: 2 runs, 10 calls, 2 errors → 20.0%
    writeln!(f, "{}", run_line(1, 4, 1)).unwrap(); // after: 3 runs, 12 calls, 3 errors → 25.0%
    writeln!(f, "{}", run_line(0, 4, 1)).unwrap(); // (1b: ≥3 after-runs to be judgeable)
    writeln!(f, "{}", run_line(2, 4, 1)).unwrap();
    drop(f);

    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager.add(marker_for("coder", 3)).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Trigger lesson",
            "agent",
            &["agent:coder"],
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

    let prompt = &prompts.lock().unwrap()[0];
    let before = prompt
        .lines()
        .find(|l| l.starts_with("metrics before the change"))
        .unwrap_or_else(|| panic!("no before-metrics line in prompt:\n{prompt}"));
    let after = prompt
        .lines()
        .find(|l| l.starts_with("metrics after the change"))
        .unwrap_or_else(|| panic!("no after-metrics line in prompt:\n{prompt}"));
    assert!(before.contains("2 run(s)"), "{before}");
    assert!(before.contains("20.0%"), "{before}");
    assert!(after.contains("3 run(s)"), "{after}");
    assert!(after.contains("25.0%"), "{after}");

    // The evidence line reports the before/after run counts.
    assert!(
        suggestions[0]
            .evidence
            .iter()
            .any(|e| e.starts_with("Effect check:") && e.contains("runs before vs after: 2 vs 3")),
        "evidence: {:?})",
        suggestions[0].evidence
    );
}

/// 2a: the deterministic effect verdict — error-rate delta thresholds and the
/// low-sample guards.
#[test]
fn test_effect_verdict_thresholds() {
    use crate::agents::metrics::MetricsSummary;
    fn sum(runs: u32, calls: u32, errors: u32) -> MetricsSummary {
        let mut s = MetricsSummary::default();
        s.runs = runs;
        s.tool_calls = calls;
        s.tool_errors = errors;
        s
    }
    let before = sum(5, 100, 30); // 30% error rate
    // 2e: min_samples (config improvement_min_samples) is the after-run floor.
    let m = 3u32;
    assert_eq!(
        effect_verdict(&before, &sum(0, 0, 0), m),
        "inconclusive (no runs after the change)"
    );
    assert_eq!(
        effect_verdict(&before, &sum(2, 100, 0), m),
        "inconclusive (low sample after the change)"
    );
    assert_eq!(
        effect_verdict(&before, &sum(3, 100, 0), m),
        "improved",
        "-30pp is clearly improved"
    );
    assert_eq!(
        effect_verdict(&before, &sum(3, 100, 30), m),
        "neutral",
        "flat error rate"
    );
    assert_eq!(
        effect_verdict(&before, &sum(3, 100, 40), m),
        "regressed",
        "+10pp is clearly regressed"
    );
    // Within ±1pp is neutral (100 calls: 30 vs 31 errors = +1pp → regressed;
    // 30 vs 30 = neutral already covered; 29 errors = -1pp → improved).
    assert_eq!(effect_verdict(&before, &sum(3, 100, 31), m), "regressed");
    assert_eq!(effect_verdict(&before, &sum(3, 100, 29), m), "improved");
    // 2e: raising the knob to 5 makes the same 3 runs a low-sample verdict
    // (the "1 task, verdict: improved" trap guard, configurable).
    assert_eq!(
        effect_verdict(&before, &sum(3, 100, 0), 5),
        "inconclusive (low sample after the change)"
    );
    // min_samples 0 is treated as 1 (a single after-run still judges).
    assert_eq!(effect_verdict(&before, &sum(1, 100, 0), 0), "improved");
}

/// 2a: the effect check PERSISTS its deterministic verdict into the
/// per-agent state (readable via list_improvement_status) and appends it to
/// the evidence line.
#[tokio::test]
async fn test_effect_check_records_verdict() {
    let _guard = MetricsDirGuard::new();
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    // Marker 3 days ago: BEFORE = [6,3], AFTER = [3,0].
    writeln!(f, "{}", run_line(5, 10, 3)).unwrap(); // before: 10 calls, 30% errors
    writeln!(f, "{}", run_line(2, 10, 0)).unwrap(); // after: 3 runs
    writeln!(f, "{}", run_line(1, 10, 0)).unwrap(); // 30 calls, 0% errors
    writeln!(f, "{}", run_line(0, 10, 0)).unwrap();
    drop(f);

    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager.add(marker_for("coder", 3)).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Trigger lesson",
            "agent",
            &["agent:coder"],
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

    // The verdict is persisted for the status view.
    let state = manager.agent_improvement_state("coder");
    assert_eq!(
        state.last_effect_verdict.as_deref(),
        Some("improved"),
        "state: {state:?}"
    );
    // ...and the evidence line names it.
    assert!(
        suggestions[0]
            .evidence
            .iter()
            .any(|e| e.starts_with("Effect check:") && e.contains("verdict: improved")),
        "evidence: {:?})",
        suggestions[0].evidence
    );
}

/// 1a: a window with no metric lines shows "(no data)" instead of being
/// omitted (the LLM must be able to tell "no data" from "good data"). The
/// AFTER window is given enough runs to be judgeable (1b) so the full
/// section renders; the BEFORE window stays empty.
#[tokio::test]
async fn test_effect_check_no_metrics_shows_no_data() {
    let _guard = MetricsDirGuard::new();
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    writeln!(f, "{}", run_line(1, 4, 1)).unwrap();
    writeln!(f, "{}", run_line(2, 4, 1)).unwrap();
    writeln!(f, "{}", run_line(0, 4, 1)).unwrap();
    drop(f);

    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager.add(marker_for("coder", 3)).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Trigger lesson",
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
    let _ = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();

    let prompt = &prompts.lock().unwrap()[0];
    let before = prompt
        .lines()
        .find(|l| l.starts_with("metrics before the change"))
        .unwrap_or_else(|| panic!("no before-metrics line in prompt:\n{prompt}"));
    let after = prompt
        .lines()
        .find(|l| l.starts_with("metrics after the change"))
        .unwrap_or_else(|| panic!("no after-metrics line in prompt:\n{prompt}"));
    assert!(before.contains("(no data)"), "{before}");
    assert!(!after.contains("(no data)"), "{after}");
    assert!(after.contains("3 run(s)"), "{after}");
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

/// 1b: below the sample floor the effect check DEFERS — a short
/// "not yet judgeable" section (no metrics block) and NO verdict recorded;
/// at/above the floor it settles — full section and a persisted verdict
/// (the after-window file is rewritten between the two passes of the test).
#[tokio::test]
async fn test_effect_check_low_sample_deferred() {
    let _guard = MetricsDirGuard::new();
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    let path = log.agent_path("coder");
    use std::io::Write;

    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager.add(marker_for("coder", 3)).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Trigger lesson",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let stats = RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 0,
    };

    // Pass 1: one run after the change -> deferred (short section, no verdict).
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", run_line(1, 4, 1)).unwrap();
    }
    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let _ = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    // Scope the lock: `let r = &prompts.lock().unwrap()[i]` extends the
    // MutexGuard to the whole function and would deadlock the second pass.
    let (prompt, deferred) = {
        let g = prompts.lock().unwrap();
        (
            g[0].clone(),
            g[0].contains("not yet judgeable") && g[0].contains("only 1 run(s)"),
        )
    };
    assert!(deferred, "expected deferred one-liner, prompt: {prompt}");
    // 1b: below the floor NO verdict is recorded.
    assert!(
        manager.agent_improvement_state("coder").last_effect_verdict.is_none(),
        "no verdict should be recorded below the sample floor"
    );

    // Pass 2: three runs after the change -> settled (full section, verdict).
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", run_line(1, 4, 1)).unwrap();
        writeln!(f, "{}", run_line(2, 4, 1)).unwrap();
        writeln!(f, "{}", run_line(2, 4, 1)).unwrap();
    }
    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let _ = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    let (prompt, deferred) = {
        let g = prompts.lock().unwrap();
        (g[1].clone(), g[1].contains("not yet judgeable"))
    };
    assert!(!deferred, "no defer expected with 3 runs, prompt: {prompt}");
    // 1b: at/above the floor the first full check records a verdict.
    assert!(
        manager.agent_improvement_state("coder").last_effect_verdict.is_some(),
        "verdict should be recorded at/above the sample floor"
    );
}
