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

// ── 2a: per-agent improvement state ──

/// 2a: the per-agent evidence gate is isolated by `agent:<name>` tags — a
/// lesson for agent A must NOT re-arm agent B's gate; agent-less (global)
/// lessons re-arm both.
#[test]
fn test_per_agent_evidence_isolation() {
    let _guard = MetricsDirGuard::new();
    let (manager, _dir) = fresh_manager();
    // Baseline: a check for each agent (so the gate starts closed).
    manager.record_agent_improvement_check("coder", false);
    manager.record_agent_improvement_check("researcher", false);

    assert!(!manager.has_new_agent_improvement_evidence("coder"));
    assert!(!manager.has_new_agent_improvement_evidence("researcher"));

    // A lesson tagged for "coder" re-arms ONLY coder.
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A coder-specific lesson",
            "test",
            &["agent:coder"],
        ))
        .unwrap();
    assert!(
        manager.has_new_agent_improvement_evidence("coder"),
        "own-tagged lesson must re-arm the agent"
    );
    assert!(
        !manager.has_new_agent_improvement_evidence("researcher"),
        "another agent's lesson must not re-arm this one"
    );

    // An agent-less (global) lesson re-arms the idle agent too.
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A global lesson",
            "test",
            &[],
        ))
        .unwrap();
    assert!(manager.has_new_agent_improvement_evidence("researcher"));
}

/// 2a: the per-agent cooldown counts only that agent's tasks, and the
/// no-op-streak backoff multiplies the base (streak 0/1: x1, 2: x2, 3: x3,
/// 4+: x4); a productive check resets the streak to x1.
#[test]
fn test_per_agent_cooldown_and_backoff() {
    let (manager, _dir) = fresh_manager();
    // Base cooldown 3.
    manager.record_agent_task_completed("coder");
    manager.record_agent_task_completed("coder");
    assert!(!manager.agent_improvement_due("coder", 3), "2/3 not due");
    manager.record_agent_task_completed("coder");
    assert!(manager.agent_improvement_due("coder", 3), "3/3 due");

    // No-op check #1 -> streak 1 (still x1): due again at 3 tasks.
    manager.record_agent_improvement_check("coder", false);
    for _ in 0..2 {
        manager.record_agent_task_completed("coder");
    }
    assert!(!manager.agent_improvement_due("coder", 3), "2/3 not due at x1");
    manager.record_agent_task_completed("coder");
    assert!(manager.agent_improvement_due("coder", 3), "3/3 due at x1");

    // No-op check #2 -> streak 2 -> base x2 = 6.
    manager.record_agent_improvement_check("coder", false);
    for _ in 0..5 {
        manager.record_agent_task_completed("coder");
    }
    assert!(!manager.agent_improvement_due("coder", 3), "5/6 not due at x2");
    manager.record_agent_task_completed("coder");
    assert!(manager.agent_improvement_due("coder", 3), "6/6 due at x2");

    // No-op check #3 -> streak 3 (x3); a productive check resets to x1.
    manager.record_agent_improvement_check("coder", false);
    manager.record_agent_improvement_check("coder", true);
    for _ in 0..2 {
        manager.record_agent_task_completed("coder");
    }
    assert!(!manager.agent_improvement_due("coder", 3), "2/3 not due after reset");
    manager.record_agent_task_completed("coder");
    assert!(manager.agent_improvement_due("coder", 3), "3/3 due after reset");

    // The reset check set the counter to 0; the 3 tasks after it brought it
    // to 3 (and the due-check above at 3/3 — not 3/9 — proves the x1 reset).
    // An untouched agent has no tasks at all.
    let state = manager.agent_improvement_state("coder");
    let other = manager.agent_improvement_state("researcher");
    assert_eq!(state.runs_since_check, 3, "3 tasks since the reset check");
    assert_eq!(state.no_op_streak, 0, "productive check reset the streak");
    assert_eq!(other.runs_since_check, 0, "untouched agent has no tasks");
}

/// 2a: the state document round-trips through a manager "restart" (the
/// per-agent fields must survive, not just the legacy global one).
#[test]
fn test_per_agent_state_persists_across_restart() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let make = || {
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        MemoryManager::new(config).unwrap()
    };

    let first = make();
    first.record_agent_task_completed("coder");
    first.record_agent_improvement_check("coder", false);
    first.record_effect_verdict("coder", "regressed");
    drop(first);

    let second = make();
    let state = second.agent_improvement_state("coder");
    assert_eq!(state.no_op_streak, 1, "streak must survive");
    assert!(state.last_check.is_some(), "last_check must survive");
    assert_eq!(state.last_effect_verdict.as_deref(), Some("regressed"));
    // And the evidence gate baselined on the persisted check: a lesson
    // written BEFORE the (now persisted) check is not new evidence.
    let mut old = MemoryEntry::new(MemoryType::Lesson, "Old lesson", "test", &["agent:coder"]);
    backdate(&mut old, 1);
    second.add(old).unwrap();
    assert!(!second.has_new_agent_improvement_evidence("coder"));
}

/// 2a: a LEGACY v1 state file (global `last_check` only, no `agents`) parses,
/// and the global timestamp serves as the per-agent fallback evidence
/// baseline until the agent has a check of its own.
#[test]
fn test_v1_state_file_compat() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let ts = chrono::Utc::now() - chrono::Duration::hours(1);
    std::fs::write(
        dir.path().join("improvement_state.json"),
        serde_json::json!({ "last_check": ts.timestamp() }).to_string(),
    )
    .unwrap();

    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();

    // A lesson from 2h ago predates the v1 baseline -> not new evidence.
    let mut old = MemoryEntry::new(MemoryType::Lesson, "Old lesson", "test", &[]);
    backdate(&mut old, 2);
    manager.add(old).unwrap();
    assert!(
        !manager.has_new_agent_improvement_evidence("coder"),
        "v1 global last_check must act as fallback baseline"
    );
    // A fresh lesson is new evidence.
    manager
        .add(MemoryEntry::new(MemoryType::Lesson, "Fresh", "test", &[]))
        .unwrap();
    assert!(manager.has_new_agent_improvement_evidence("coder"));
}

// ── 2f: wall-clock floor (improvement_min_interval_hours) ──

/// 2f test helper: backdate the persisted improvement check by N hours —
/// either the per-agent `last_check` (unix ms) or the legacy v1 GLOBAL one
/// (unix seconds, `agent = None`). The state doc is private to the manager,
/// so the file is edited as plain JSON.
fn backdate_check(dir: &std::path::Path, agent: Option<&str>, hours: i64) {
    let path = dir.join("improvement_state.json");
    let content = std::fs::read_to_string(&path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&content).unwrap();
    match agent {
        Some(a) => {
            let ms = (chrono::Utc::now() - chrono::Duration::hours(hours)).timestamp_millis();
            v["agents"][a]["last_check"] = serde_json::json!(ms);
        }
        None => {
            let secs = (chrono::Utc::now() - chrono::Duration::hours(hours)).timestamp();
            v["last_check"] = serde_json::json!(secs);
        }
    }
    std::fs::write(&path, v.to_string()).unwrap();
}

/// 2f: with a wall-clock floor configured, passing the task-count gate is
/// NOT enough — the check is only due once the floor has elapsed since the
/// agent's last check; the task-count gate still rules on its own.
#[test]
fn test_min_interval_hours_gates_due() {
    let dir = tempdir().unwrap();
    let make = |hours: u32| {
        let config = MemoryConfig {
            improvement_min_interval_hours: hours,
            improvement_cooldown_tasks: 1,
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        MemoryManager::new(config).unwrap()
    };

    // 0h (the default) → the legacy pure task-count gate: 1 task, base 1 → due.
    let m = make(0);
    m.record_agent_task_completed("coder");
    assert!(m.agent_improvement_due("coder", 1), "0h: no wall-clock gate");

    // 1h floor: a check recorded just now + 3 tasks (task gate 3/1 passes)
    // → still NOT due (the wall-clock gate holds it back).
    let m = make(1);
    m.record_agent_improvement_check("coder", true);
    for _ in 0..3 {
        m.record_agent_task_completed("coder");
    }
    assert!(!m.agent_improvement_due("coder", 1), "fresh check + 1h floor → not due");

    // Backdate the check 2h → the floor has elapsed → due.
    backdate_check(dir.path(), Some("coder"), 2);
    assert!(m.agent_improvement_due("coder", 1), "floor elapsed → due");

    // The floor never OVERRIDES the task-count gate: 0 tasks since the check
    // → not due, even with an ancient check.
    let m = make(1);
    m.record_agent_improvement_check("coder", true);
    backdate_check(dir.path(), Some("coder"), 48);
    assert!(!m.agent_improvement_due("coder", 1), "task-count gate still rules");
}

/// 2f: the floor falls back to the legacy v1 GLOBAL baseline when the agent
/// has no check of its own (the same rule as the evidence gate).
#[test]
fn test_min_interval_uses_v1_fallback_baseline() {
    let dir = tempdir().unwrap();
    // v1 state: only a global last_check (unix seconds), 1h old.
    std::fs::write(
        dir.path().join("improvement_state.json"),
        serde_json::json!({
            "version": 1,
            "last_check": (chrono::Utc::now() - chrono::Duration::hours(1)).timestamp(),
        })
        .to_string(),
    )
    .unwrap();

    let config = MemoryConfig {
        improvement_min_interval_hours: 2,
        improvement_cooldown_tasks: 1,
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    manager.record_agent_task_completed("coder");

    // v1 baseline is 1h old < 2h floor → not due (task gate 1/1 passes).
    assert!(!manager.agent_improvement_due("coder", 1), "v1 baseline within floor → not due");

    // Backdate the v1 baseline 3h → the floor has elapsed → due.
    backdate_check(dir.path(), None, 3);
    assert!(manager.agent_improvement_due("coder", 1), "v1 baseline past floor → due");
}

// ── 1a: metrics-driven evidence gate ──

/// One JSON metrics `run` line backdated `hours_ago` hours (written directly —
/// `MetricsLine`'s variant fields are not constructible outside the metrics
/// module). `gave_up` picks the terminal outcome (vs `verified`); calls/errors
/// set the error-rate; duration is fixed at 10s so flat/error/gave-up tests
/// show no duration delta.
fn run_line_hours(hours_ago: i64, calls: u32, errors: u32, gave_up: bool) -> String {
    serde_json::json!({
        "kind": "run",
        "ts": (chrono::Utc::now() - chrono::Duration::hours(hours_ago)).to_rfc3339(),
        "tool_calls": calls,
        "tool_errors": errors,
        "verification_attempts": 1,
        "duration_ms": 10_000,
        "outcome": if gave_up { "gave_up" } else { "verified" },
    })
    .to_string()
}

/// Write raw-JSON run lines into the (temp) metrics log for an agent (the same
/// direct-write pattern as the effect tests — `MetricsLine`'s variant fields
/// are not constructible outside the metrics module).
fn write_run_lines(
    log: &crate::agents::metrics::MetricsLog,
    agent: &str,
    lines: &[String],
) {
    use std::io::Write;
    // Append (not create/truncate) so the before- and after-line sets both
    // land in the file when written in separate calls.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log.agent_path(agent))
        .unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
}

/// 1a test setup: a manager whose baseline (last check) is backdated 2 days,
/// so `agent_metric_evidence` computes the after-window as [2d, now] and the
/// before-window as [4d, 2d] (the same before-window rule as the effect check).
/// Lines placed at ~3d ago land in the before-window; ~24h ago in the after.
fn metric_gate_manager(dir: &std::path::Path) -> MemoryManager {
    let config = MemoryConfig {
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    manager.record_agent_improvement_check("coder", false);
    // Backdate the baseline 2 days (unix ms in the per-agent state doc).
    let path = dir.join("improvement_state.json");
    let content = std::fs::read_to_string(&path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&content).unwrap();
    v["agents"]["coder"]["last_check"] =
        serde_json::json!((chrono::Utc::now() - chrono::Duration::days(2)).timestamp_millis());
    std::fs::write(&path, v.to_string()).unwrap();
    manager
}

/// Build before/after line sets: `n_before` lines at ~3d ago (before-window)
/// and `n_after` lines at ~24h ago (after-window), each side with its own
/// (tool_calls, tool_errors, gave_up) profile.
fn before_after_lines(
    n_before: usize,
    b: (u32, u32, bool),
    n_after: usize,
    a: (u32, u32, bool),
) -> (Vec<String>, Vec<String>) {
    let before = (0..n_before)
        .map(|i| run_line_hours(72i64 - i as i64, b.0, b.1, b.2))
        .collect();
    let after = (0..n_after)
        .map(|i| run_line_hours(25i64 - i as i64, a.0, a.1, a.2))
        .collect();
    (before, after)
}

/// 1a: FLAT metrics since the last check (run floor met, but no delta) must
/// NOT re-arm the gate — the loop wakes only on data that actually changed.
#[test]
fn test_metric_evidence_flat_no_rearm() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let manager = metric_gate_manager(dir.path());
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    // Flat before and after: 10% tool errors, verified.
    let (before, after) = before_after_lines(4, (10, 1, false), 4, (10, 1, false));
    write_run_lines(&log, "coder", &before);
    write_run_lines(&log, "coder", &after);
    assert!(
        manager.agent_metric_evidence("coder").is_none(),
        "flat metrics (floor met, no delta) must not re-arm"
    );
    assert!(!manager.has_new_agent_improvement_evidence("coder"));
}

/// 1a: a tool-error-rate JUMP since the last check (floor met) re-arms the
/// gate even with no new lesson — the loop wakes on data, not just complaints.
#[test]
fn test_metric_evidence_error_jump_rearms() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let manager = metric_gate_manager(dir.path());
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    // Jump after: 80% tool errors, still verified (isolates the error-rate
    // path from the gave-up path).
    let (before, after) = before_after_lines(4, (10, 1, false), 4, (10, 8, false));
    write_run_lines(&log, "coder", &before);
    write_run_lines(&log, "coder", &after);
    let reason = manager.agent_metric_evidence("coder");
    assert!(reason.is_some(), "error-rate jump must re-arm: {reason:?}");
    assert!(manager.has_new_agent_improvement_evidence("coder"));
    let reason = reason.expect("checked is_some above");
    assert!(
        reason.contains("tool-error rate"),
        "reason names the delta: {reason}"
    );
}

/// 1a: a gave-up-share JUMP (floor met) re-arms too — the second delta path,
/// alongside error-rate and duration.
#[test]
fn test_metric_evidence_gave_up_jump_rearms() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let manager = metric_gate_manager(dir.path());
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    // Error-rate kept flat (1/10) so the gave-up share 0→100% is the only
    // delta that fires.
    let (before, after) = before_after_lines(4, (10, 1, false), 4, (10, 1, true));
    write_run_lines(&log, "coder", &before);
    write_run_lines(&log, "coder", &after);
    let reason = manager.agent_metric_evidence("coder").unwrap();
    assert!(
        reason.contains("gave-up share"),
        "reason names the gave-up delta: {reason}"
    );
}

/// 1a: below the run floor, even a big delta must NOT re-arm — 1–2 noisy runs
/// shouldn't burn an LLM call.
#[test]
fn test_metric_evidence_below_run_floor_no_rearm() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let manager = metric_gate_manager(dir.path());
    let log = crate::agents::metrics::MetricsLog::new(_guard.dir());
    // Jump after but only 2 runs (< default floor 3).
    let (before, after) = before_after_lines(4, (10, 1, false), 2, (10, 8, false));
    write_run_lines(&log, "coder", &before);
    write_run_lines(&log, "coder", &after);
    assert!(
        manager.agent_metric_evidence("coder").is_none(),
        "below run floor, no re-arm even with a big delta"
    );
    assert!(!manager.has_new_agent_improvement_evidence("coder"));
}

/// 1a: the Lesson gate stays the fast path — a new lesson re-arms the agent
/// even when there are NO metrics at all (the metric half is None).
#[test]
fn test_metric_evidence_lessons_only_still_arms() {
    let _guard = MetricsDirGuard::new();
    let dir = tempdir().unwrap();
    let manager = metric_gate_manager(dir.path());
    assert!(
        manager.agent_metric_evidence("coder").is_none(),
        "no metrics -> no metric evidence"
    );
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A fresh coder lesson (no metrics involved)",
            "test",
            &["agent:coder"],
        ))
        .unwrap();
    assert!(
        manager.has_new_agent_improvement_evidence("coder"),
        "lesson-only must still re-arm (fast path)"
    );
}
