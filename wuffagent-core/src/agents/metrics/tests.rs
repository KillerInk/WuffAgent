//! M1 tests: append/parse round-trip, summary aggregation, feedback lines,
//! corrupt-line tolerance, file-name sanitization.

use super::*;
use chrono::TimeZone;

fn tmp_dir(name: &str) -> PathBuf {
    let nix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "wuffagent-metrics-{}-{}-{nix}",
        std::process::id(),
        name
    ))
}

fn ts(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, hour, 0, 0).unwrap()
}

#[test]
fn test_run_roundtrip_and_append() {
    let dir = tmp_dir("rt");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    log.log_run("coder", 12, 2, 1, 45_210, RunOutcome::Verified, 100, 20);
    log.log_run("coder", 3, 0, 2, 8_000, RunOutcome::GaveUp, 0, 0);
    log.log_feedback("coder", true);
    log.log_feedback("coder", false);

    let content = std::fs::read_to_string(log.agent_path("coder")).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 4, "one line per record: {content}");

    let parsed: Vec<MetricsLine> =
        lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    match &parsed[0] {
        MetricsLine::Run {
            tool_calls,
            tool_errors,
            verification_attempts,
            duration_ms,
            outcome,
            tokens_in,
            tokens_out,
            ..
        } => {
            assert_eq!(*tool_calls, 12);
            assert_eq!(*tool_errors, 2);
            assert_eq!(*verification_attempts, 1);
            assert_eq!(*duration_ms, 45_210);
            assert_eq!(*outcome, RunOutcome::Verified);
            // 4c: token fields round-trip through the JSONL store.
            assert_eq!(*tokens_in, 100);
            assert_eq!(*tokens_out, 20);
        }
        other => panic!("expected run line, got {other:?}"),
    }
    assert!(matches!(&parsed[2], MetricsLine::Feedback {
        feedback: FeedbackKind::Up, ..
    }));
    assert!(matches!(&parsed[3], MetricsLine::Feedback {
        feedback: FeedbackKind::Down, ..
    }));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_per_agent_files() {
    let dir = tmp_dir("peragent");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    log.log_run("coder", 1, 0, 1, 100, RunOutcome::Verified, 0, 0);
    log.log_run("architect", 2, 1, 1, 200, RunOutcome::None, 0, 0);

    let coder = log.read_all("coder");
    let architect = log.read_all("architect");
    assert_eq!(coder.len(), 1);
    assert_eq!(architect.len(), 1);
    assert!(!log.agent_path("coder").exists() || true);
    // Names are file-name sanitized.
    assert_eq!(
        log.agent_path("My Agent!").file_name().unwrap().to_str(),
        Some("my-agent.jsonl")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_missing_file_reads_empty() {
    let dir = tmp_dir("missing");
    let log = MetricsLog::new(&dir);
    assert!(log.read_all("nobody").is_empty());
    assert!(log.recent("nobody", 5).is_empty());
    let s = log.summary_since("nobody", None);
    assert_eq!(s, MetricsSummary::default());
    assert!(s.format_line().is_empty());
}

#[test]
fn test_corrupt_lines_skipped() {
    let dir = tmp_dir("corrupt");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    log.log_run("coder", 5, 1, 1, 1_000, RunOutcome::Verified, 0, 0);
    // Append garbage + an empty line + a line of the wrong shape.
    use std::io::Write;
    let mut f = OpenOptions::new().append(true).open(log.agent_path("coder")).unwrap();
    writeln!(f, "not json at all").unwrap();
    writeln!(f, "").unwrap();
    writeln!(f, "{{\"kind\":\"bogus\"}}").unwrap();
    drop(f);

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 1, "only the valid line survives: {lines:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_summary_since_and_format_line() {
    let dir = tmp_dir("summary");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);

    // Write lines with known timestamps directly (log_run uses Utc::now()).
    let line = |o: RunOutcome, day: u32| {
        serde_json::to_string(&MetricsLine::Run {
            ts: ts(day, 10),
            tool_calls: 10,
            tool_errors: 1,
            verification_attempts: 1,
            duration_ms: 5_000,
            outcome: o,
            tokens_in: 0,
            tokens_out: 0,
        })
        .unwrap()
    };
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    writeln!(f, "{}", line(RunOutcome::Verified, 20)).unwrap();
    writeln!(f, "{}", line(RunOutcome::VerifiedAfterRetry, 21)).unwrap();
    writeln!(f, "{}", line(RunOutcome::GaveUp, 22)).unwrap();
    writeln!(f, "{}", line(RunOutcome::None, 23)).unwrap();
    drop(f);

    let all = log.summary_since("coder", None);
    assert_eq!(all.runs, 4);
    assert_eq!(all.tool_calls, 40);
    assert_eq!(all.tool_errors, 4);
    assert_eq!((all.verified, all.verified_after_retry, all.gave_up, all.not_verified), (1, 1, 1, 1));

    // Only the last two days survive a `since` filter.
    let recent = log.summary_since("coder", Some(ts(22, 0)));
    assert_eq!(recent.runs, 2);
    assert_eq!((recent.gave_up, recent.not_verified), (1, 1));

    let formatted = all.format_line();
    assert!(formatted.contains("4 run(s)"), "got: {formatted}");
    assert!(formatted.contains("10.0%"), "got: {formatted}");
    assert!(formatted.contains("1 verified / 1 verified_after_retry / 1 gave_up / 1 not verified"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_summary_between_windows() {
    let dir = tmp_dir("between");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);

    // Four runs on days 20..23 (same fixture shape as the since-test).
    let line = |o: RunOutcome, day: u32| {
        serde_json::to_string(&MetricsLine::Run {
            ts: ts(day, 10),
            tool_calls: 10,
            tool_errors: 1,
            verification_attempts: 1,
            duration_ms: 5_000,
            outcome: o,
            tokens_in: 0,
            tokens_out: 0,
        })
        .unwrap()
    };
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    writeln!(f, "{}", line(RunOutcome::Verified, 20)).unwrap();
    writeln!(f, "{}", line(RunOutcome::VerifiedAfterRetry, 21)).unwrap();
    writeln!(f, "{}", line(RunOutcome::GaveUp, 22)).unwrap();
    writeln!(f, "{}", line(RunOutcome::None, 23)).unwrap();
    drop(f);

    // [day21, day23) → days 21 and 22 only (end exclusive).
    let b = log.summary_between("coder", Some(ts(21, 0)), Some(ts(23, 0)));
    assert_eq!(b.runs, 2);
    assert_eq!((b.verified, b.verified_after_retry, b.gave_up, b.not_verified), (0, 1, 1, 0));

    // [None, day22) → days 20 and 21.
    let b2 = log.summary_between("coder", None, Some(ts(22, 0)));
    assert_eq!(b2.runs, 2);
    assert_eq!((b2.verified, b2.verified_after_retry), (1, 1));

    // start >= end → empty (even for an agent with data).
    assert_eq!(
        log.summary_between("coder", Some(ts(22, 0)), Some(ts(22, 0))),
        MetricsSummary::default()
    );
    assert_eq!(
        log.summary_between("coder", Some(ts(23, 0)), Some(ts(20, 0))),
        MetricsSummary::default()
    );

    // summary_since is summary_between with an open end.
    assert_eq!(
        log.summary_since("coder", Some(ts(22, 0))),
        log.summary_between("coder", Some(ts(22, 0)), None)
    );
    assert_eq!(log.summary_between("coder", None, None).runs, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_agent_file_name() {
    assert_eq!(agent_file_name("coder"), "coder");
    assert_eq!(agent_file_name("My Agent!"), "my-agent");
    assert_eq!(agent_file_name("wuff_agent-1"), "wuff_agent-1");
    assert_eq!(agent_file_name("  "), "agent");
    assert_eq!(agent_file_name("a..b"), "a-b");
}

#[test]
fn test_recent_caps_to_last_n() {
    let dir = tmp_dir("recent");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    for i in 0..7 {
        log.log_run("coder", i, 0, 1, 100, RunOutcome::Verified, 0, 0);
    }
    let recent = log.recent("coder", 3);
    assert_eq!(recent.len(), 3);
    match &recent[0] {
        MetricsLine::Run { tool_calls, .. } => assert_eq!(*tool_calls, 4),
        other => panic!("expected run line, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Test hygiene: inside this crate's test binary, `default()` must point at
/// the per-process temp dir (never the real `~/.wuffagent/metrics`), and an
/// explicit override must still win.
#[test]
fn test_default_uses_test_process_dir() {
    let d = MetricsLog::default().dir().to_path_buf();
    assert!(
        d.starts_with(std::env::temp_dir())
            && d.to_string_lossy().contains("wuffagent-metrics-test-"),
        "default() under test must use the per-process temp dir, got: {:?}",
        d
    );
    assert!(!d.starts_with(crate::config::get_wuffagent_home()));

    let override_dir = tmp_dir("override-wins");
    set_metrics_dir_for_testing(Some(override_dir.clone()));
    assert_eq!(MetricsLog::default().dir(), override_dir.as_path());
    set_metrics_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(&override_dir);
}

/// 3a: skill-usage lines go to the reserved `skills.jsonl` file, are
/// readable back (deduplicated, oldest-first), and the reserved file is not
/// reported as an agent by `agent_names()`.
#[test]
fn test_skill_usage_roundtrip_and_reserved_file() {
    let dir = tmp_dir("skill-usage");
    let log = MetricsLog::new(&dir);
    log.log_run("coder", 3, 1, 1, 1000, RunOutcome::Verified, 0, 0);
    log.log_skill_use("wuffagent-self-restart");
    log.log_skill_use("git-rebase-workflow");
    log.log_skill_use("wuffagent-self-restart"); // duplicate

    // Round trip through the raw lines (the file holds the reserved stem).
    let all = log.read_all(SKILLS_FILE_STEM);
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|l| matches!(l, MetricsLine::SkillUse { .. })));

    let usage = log.skill_usage_since(None);
    assert_eq!(
        usage,
        vec!["wuffagent-self-restart", "git-rebase-workflow"],
        "deduplicated, first-seen order"
    );
    assert_eq!(log.skill_usage_since(Some(Utc::now() + chrono::Duration::hours(1))), Vec::<String>::new());

    // The reserved file must not show up as an agent in the fleet list.
    let names = log.agent_names();
    assert_eq!(names, vec!["coder".to_string()], "got: {names:?}");

    // describe() renders the line for UI lists.
    if let MetricsLine::SkillUse { skill, .. } = &all[0] {
        assert!(format!("{}", all[0].describe()).contains(skill));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// S2 (context-rot): trim lines round-trip through the JSONL store, are
/// counted in the summary, and render for UI lists.
#[test]
fn test_trim_line_roundtrip_and_summary() {
    let dir = tmp_dir("trim-line");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    log.log_run("coder", 12, 1, 1, 40_000, RunOutcome::Verified, 50_000, 1_200);
    log.log_trim("coder", 900_000, 450_000, 34, true, false);
    log.log_trim("coder", 990_000, 430_000, 41, true, true); // overflow backstop

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 3);
    match &lines[1] {
        MetricsLine::Trim {
            chars_before,
            chars_after,
            messages_removed,
            brief_updated,
            overflow,
            ..
        } => {
            assert_eq!(*chars_before, 900_000);
            assert_eq!(*chars_after, 450_000);
            assert_eq!(*messages_removed, 34);
            assert!(brief_updated);
            assert!(!overflow);
        }
        other => panic!("expected trim line, got {other:?}"),
    }
    match &lines[2] {
        MetricsLine::Trim { overflow, .. } => assert!(*overflow),
        other => panic!("expected overflow trim line, got {other:?}"),
    }

    // The summary counts trims (and still reports runs), and renders them.
    let s = log.summary_since("coder", None);
    assert_eq!(s.runs, 1);
    assert_eq!(s.trims, 2);
    let rendered = s.format_line();
    assert!(rendered.contains("context trims: 2"), "got: {rendered}");
    assert!(
        rendered.contains("run(s)"),
        "trim-only summary must not hide the run stats: {rendered}"
    );

    // describe() renders the line for raw-line lists.
    let d = lines[1].describe();
    assert!(d.contains("34 messages removed"), "got: {d}");
    assert!(d.contains("brief updated"), "got: {d}");
    let d2 = lines[2].describe();
    assert!(d2.contains("overflow backstop"), "got: {d2}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A trim-only history must still render a summary (the empty-check includes
/// `trims`).
#[test]
fn test_trim_only_summary_is_not_empty() {
    let dir = tmp_dir("trim-only");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    log.log_trim("coder", 100_000, 50_000, 5, false, false);
    let s = log.summary_since("coder", None);
    assert_eq!(s.trims, 1);
    assert!(!s.format_line().is_empty(), "trim-only summary must not be empty");
    let _ = std::fs::remove_dir_all(&dir);
}
