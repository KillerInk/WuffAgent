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
    log.log_run("coder", 12, 2, 1, 45_210, RunOutcome::Verified);
    log.log_run("coder", 3, 0, 2, 8_000, RunOutcome::GaveUp);
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
            ..
        } => {
            assert_eq!(*tool_calls, 12);
            assert_eq!(*tool_errors, 2);
            assert_eq!(*verification_attempts, 1);
            assert_eq!(*duration_ms, 45_210);
            assert_eq!(*outcome, RunOutcome::Verified);
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
    log.log_run("coder", 1, 0, 1, 100, RunOutcome::Verified);
    log.log_run("architect", 2, 1, 1, 200, RunOutcome::None);

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
    log.log_run("coder", 5, 1, 1, 1_000, RunOutcome::Verified);
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
        log.log_run("coder", i, 0, 1, 100, RunOutcome::Verified);
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
