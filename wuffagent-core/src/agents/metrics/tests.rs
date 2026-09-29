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

/// 1a test helper: a RunStats with scalar counters only (no tool histogram).
fn rs(calls: u32, errors: u32, attempts: u32) -> RunStats {
    RunStats {
        tool_calls: calls as usize,
        tool_errors: errors as usize,
        verification_attempts: attempts,
        ..Default::default()
    }
}

/// Serializes the tests that read or write the process-global test override
/// (`set_metrics_dir_for_testing`) — the SAME shared lock the improvement
/// tests' MetricsDirGuard holds: `test_default_uses_test_process_dir`
/// asserts the default is the per-process dir, which only holds while no
/// parallel test has set an override.
fn metrics_override_lock() -> std::sync::MutexGuard<'static, ()> {
    super::METRICS_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 1a: the per-tool histogram round-trips on run lines (bump_tool feeds it),
/// and a legacy line written before 1a (no `tools` field) parses with an
/// empty histogram.
#[test]
fn test_run_tools_roundtrip_and_legacy_default() {
    let dir = tmp_dir("tools");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);

    // A fresh RunStats fed through bump_tool (scalars + histogram agree).
    let mut stats = RunStats::default();
    stats.bump_tool("shell", false, 1_200);
    stats.bump_tool("shell", true, 800);
    stats.bump_tool("read_file", false, 40);
    stats.verification_attempts = 1;
    log.log_run("coder", &stats, 5_000, RunOutcome::Verified, 10, 2, "run-1", "sess-1");

    // A legacy line without the `tools` field (pre-1a store format).
    use std::io::Write;
    let mut f = OpenOptions::new()
        .append(true)
        .open(log.agent_path("coder"))
        .unwrap();
    writeln!(
        f,
        r#"{{"kind":"run","ts":"2026-09-01T00:00:00.000Z","tool_calls":2,"tool_errors":0,"verification_attempts":1,"duration_ms":100,"outcome":"verified","tokens_in":0,"tokens_out":0}}"#
    )
    .unwrap();
    drop(f);

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 2, "got: {lines:?}");
    match &lines[0] {
        MetricsLine::Run {
            tool_calls,
            tool_errors,
            tools,
            ..
        } => {
            assert_eq!(*tool_calls, 3);
            assert_eq!(*tool_errors, 1);
            assert_eq!(tools.len(), 2);
            let shell = tools.iter().find(|t| t.name == "shell").unwrap();
            assert_eq!(
                (shell.calls, shell.errors, shell.duration_ms),
                (2, 1, 2_000)
            );
            let rf = tools.iter().find(|t| t.name == "read_file").unwrap();
            assert_eq!((rf.calls, rf.errors, rf.duration_ms), (1, 0, 40));
        }
        other => panic!("expected run line, got {other:?}"),
    }
    match &lines[1] {
        MetricsLine::Run { tools, .. } => assert!(
            tools.is_empty(),
            "legacy line must parse with an empty histogram"
        ),
        other => panic!("expected run line, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1e: run_id/session_id round-trip on Run and Trim lines — the join key
/// tying a run's Run line to its Trim lines and to the usage.jsonl
/// entries. Legacy lines written before 1e parse with empty ids.
#[test]
fn test_run_ids_roundtrip_and_legacy_default() {
    let dir = tmp_dir("run-ids");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);

    log.log_run("coder", &rs(2, 0, 1), 5_000, RunOutcome::Verified, 10, 2, "run-abc", "sess-xyz");
    log.log_trim("coder", 1_000, 400, 3, true, false, "run-abc");

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 2, "got: {lines:?}");
    match &lines[0] {
        MetricsLine::Run {
            run_id,
            session_id,
            ..
        } => {
            assert_eq!(run_id, "run-abc");
            assert_eq!(session_id, "sess-xyz");
        }
        other => panic!("expected run line, got {other:?}"),
    }
    match &lines[1] {
        MetricsLine::Trim { run_id, .. } => assert_eq!(run_id, "run-abc"),
        other => panic!("expected trim line, got {other:?}"),
    }

    // A pre-1e legacy run line (no run_id/session_id) parses with "".
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("legacyagent")).unwrap();
    writeln!(
        f,
        r#"{{"kind":"run","ts":"2026-09-01T00:00:00.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":0,"duration_ms":100,"outcome":"verified","tokens_in":0,"tokens_out":0}}"#
    )
    .unwrap();
    drop(f);
    let legacy = log.read_all("legacyagent");
    assert_eq!(legacy.len(), 1, "got: {legacy:?}");
    match &legacy[0] {
        MetricsLine::Run {
            run_id,
            session_id,
            ..
        } => {
            assert_eq!(run_id, "");
            assert_eq!(session_id, "");
        }
        other => panic!("expected run line, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1d: log_run writes the duration split — `llm_ms` straight from
/// RunStats (the loop folds the judge's time in before the write) and
/// `tools_ms` as the per-tool histogram's sum. A legacy line (pre-1d)
/// deserializes to 0/0, and the split never exceeds the wall-clock total.
#[test]
fn test_run_duration_split_roundtrip_and_legacy_default() {
    let dir = tmp_dir("split");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);

    let mut stats = rs(0, 0, 1);
    stats.llm_ms = 4_000; // main rounds (the judge's time would be folded in here)
    stats.bump_tool("shell", false, 2_000);
    stats.bump_tool("read_file", true, 500);

    log.log_run("coder", &stats, 7_000, RunOutcome::Verified, 10, 2, "run-1", "sess-1");

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 1, "got: {lines:?}");
    match &lines[0] {
        MetricsLine::Run {
            llm_ms,
            tools_ms,
            duration_ms,
            ..
        } => {
            assert_eq!(*llm_ms, 4_000);
            assert_eq!(*tools_ms, 2_500, "must be the histogram's sum");
            // The split can never exceed the wall-clock total.
            assert!(
                *llm_ms + *tools_ms <= *duration_ms,
                "llm {llm_ms:?} + tools {tools_ms:?} > wall {duration_ms:?}"
            );
        }
        other => panic!("expected run line, got {other:?}"),
    }

    // A pre-1d legacy run line (no llm_ms/tools_ms) parses with 0/0.
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("legacyagent")).unwrap();
    writeln!(
        f,
        r#"{{"kind":"run","ts":"2026-09-01T00:00:00.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":0,"duration_ms":100,"outcome":"verified","tokens_in":0,"tokens_out":0}}"#
    )
    .unwrap();
    drop(f);
    let legacy = log.read_all("legacyagent");
    assert_eq!(legacy.len(), 1, "got: {legacy:?}");
    match &legacy[0] {
        MetricsLine::Run {
            llm_ms,
            tools_ms,
            ..
        } => {
            assert_eq!(*llm_ms, 0);
            assert_eq!(*tools_ms, 0);
        }
        other => panic!("expected run line, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1b: log_run writes the model label + estimated cost straight from
/// RunStats (the loop stamps them from the client's last model and the app's
/// price table before the write); a legacy line (pre-1b) deserializes to
/// model "" and cost 0.0.
#[test]
fn test_run_model_and_cost_roundtrip_and_legacy_default() {
    let dir = tmp_dir("model-cost");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);

    let mut stats = rs(0, 0, 1);
    stats.model = "gpt-4o-mini".to_string();
    stats.cost_usd = 0.00023;
    log.log_run("coder", &stats, 5_000, RunOutcome::Verified, 10, 2, "run-1", "sess-1");

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 1, "got: {lines:?}");
    match &lines[0] {
        MetricsLine::Run { model, cost_usd, .. } => {
            assert_eq!(model, "gpt-4o-mini");
            assert!((*cost_usd - 0.00023).abs() < 1e-12, "got: {cost_usd}");
        }
        other => panic!("expected run line, got {other:?}"),
    }

    // A pre-1b legacy run line (no model/cost_usd) parses to ""/0.0.
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("legacyagent")).unwrap();
    writeln!(
        f,
        r#"{{"kind":"run","ts":"2026-09-01T00:00:00.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":0,"duration_ms":100,"outcome":"verified","tokens_in":0,"tokens_out":0}}"#
    )
    .unwrap();
    drop(f);
    let legacy = log.read_all("legacyagent");
    assert_eq!(legacy.len(), 1, "got: {legacy:?}");
    match &legacy[0] {
        MetricsLine::Run { model, cost_usd, .. } => {
            assert_eq!(model, "");
            assert_eq!(*cost_usd, 0.0);
        }
        other => panic!("expected run line, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1a: bump_tool aggregates calls/errors/summed ms per tool, and the 33rd
/// distinct tool name folds into the "__other__" bucket (run lines stay
/// bounded regardless of how many tools a run touches).
#[test]
fn test_bump_tool_aggregation_and_fold() {
    let mut stats = RunStats::default();
    for i in 0..33 {
        stats.bump_tool(&format!("tool{i}"), false, 10);
    }
    // 32 distinct tools kept, the 33rd (tool32) folded.
    assert_eq!(stats.tools.len(), 33, "32 named + __other__");
    assert_eq!(stats.tool_calls, 33);
    assert!(
        stats.tools.iter().any(|t| t.name == "tool0" && t.calls == 1 && t.duration_ms == 10)
    );
    let other = stats
        .tools
        .iter()
        .find(|t| t.name == "__other__")
        .expect("__other__ bucket");
    assert_eq!(other.calls, 1);
    assert_eq!(other.duration_ms, 10);

    // A second call of an EXISTING tool never re-folds, even past 32 names.
    stats.bump_tool("tool0", true, 5);
    let t0 = stats.tools.iter().find(|t| t.name == "tool0").unwrap();
    assert_eq!((t0.calls, t0.errors, t0.duration_ms), (2, 1, 15));
    assert_eq!(stats.tool_calls, 34);
    assert_eq!(stats.tool_errors, 1);
    assert_eq!(stats.tools.len(), 33);
}

#[test]
fn test_run_roundtrip_and_append() {
    let dir = tmp_dir("rt");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    log.log_run("coder", &rs(12, 2, 1), 45_210, RunOutcome::Verified, 100, 20, "run-1", "sess-1");
    log.log_run("coder", &rs(3, 0, 2), 8_000, RunOutcome::GaveUp, 0, 0, "run-1", "sess-1");
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
    log.log_run("coder", &rs(1, 0, 1), 100, RunOutcome::Verified, 0, 0, "run-1", "sess-1");
    log.log_run("architect", &rs(2, 1, 1), 200, RunOutcome::None, 0, 0, "run-1", "sess-1");

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
    log.log_run("coder", &rs(5, 1, 1), 1_000, RunOutcome::Verified, 0, 0, "run-1", "sess-1");
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
            tools: Vec::new(),
            run_id: String::new(),
            session_id: String::new(),
llm_ms: 0,
tools_ms: 0,
model: String::new(),
cost_usd: 0.0,
v: 1,
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
            tools: Vec::new(),
            run_id: String::new(),
            session_id: String::new(),
llm_ms: 0,
tools_ms: 0,
model: String::new(),
cost_usd: 0.0,
v: 1,
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

/// The raw-line twin of `summary_since`: same window semantics (inclusive
/// `since`, `None` = all time), but the un-aggregated lines, oldest first,
/// across line kinds.
#[test]
fn test_lines_since_window() {
    let dir = tmp_dir("lines_since");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);

    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    use std::io::Write;
    // Runs on days 20 and 22, a feedback line on day 21 (mixed kinds).
    for day in [20, 22] {
        writeln!(
            f,
            "{}",
            serde_json::to_string(&MetricsLine::Run {
                ts: ts(day, 10),
                tool_calls: 2,
                tool_errors: 0,
                verification_attempts: 1,
                duration_ms: 5_000,
                outcome: RunOutcome::Verified,
                tokens_in: 0,
                tokens_out: 0,
                tools: Vec::new(),
            run_id: String::new(),
            session_id: String::new(),
llm_ms: 0,
tools_ms: 0,
model: String::new(),
cost_usd: 0.0,
v: 1,
            })
            .unwrap()
        )
        .unwrap();
    }
    writeln!(
        f,
        "{}",
        serde_json::to_string(&MetricsLine::Feedback {
            ts: ts(21, 9),
            feedback: FeedbackKind::Up,
            run_id: None,
            v: 1,
        })
        .unwrap()
    )
    .unwrap();
    drop(f);

    // None = everything, oldest first (runs 20, 22 + feedback 21 as stored).
    let all = log.lines_since("coder", None);
    assert_eq!(all.len(), 3);

    // since day 21 → the feedback (21,9) and the day-22 run; the day-20 run
    // is strictly before. Oldest first.
    let mid = log.lines_since("coder", Some(ts(21, 0)));
    assert_eq!(mid.len(), 2);
    assert!(matches!(&mid[0], MetricsLine::Feedback { .. }), "{mid:?}");
    assert!(matches!(&mid[1], MetricsLine::Run { .. }), "{mid:?}");

    // Inclusive boundary: since exactly the day-22 run's ts keeps it.
    let edge = log.lines_since("coder", Some(ts(22, 10)));
    assert_eq!(edge.len(), 1);
    assert!(matches!(&edge[0], MetricsLine::Run { .. }), "{edge:?}");

    // A future `since` yields nothing; an unknown agent too.
    assert!(log.lines_since("coder", Some(ts(30, 0))).is_empty());
    assert!(log.lines_since("ghost", None).is_empty());
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
        log.log_run("coder", &rs(i as u32, 0, 1), 100, RunOutcome::Verified, 0, 0, "run-1", "sess-1");
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
    let _lock = metrics_override_lock();
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
    log.log_run("coder", &rs(3, 1, 1), 1000, RunOutcome::Verified, 0, 0, "run-1", "sess-1");
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
    log.log_run("coder", &rs(12, 1, 1), 40_000, RunOutcome::Verified, 50_000, 1_200, "run-1", "sess-1");
    log.log_trim("coder", 900_000, 450_000, 34, true, false, "run-1");
    log.log_trim("coder", 990_000, 430_000, 41, true, true, "run-1"); // overflow backstop

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
    log.log_trim("coder", 100_000, 50_000, 5, false, false, "");
    let s = log.summary_since("coder", None);
    assert_eq!(s.trims, 1);
    assert!(!s.format_line().is_empty(), "trim-only summary must not be empty");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1c: check lines (the loop's own cost) round-trip through the JSONL store —
/// an agent-scoped check lands in the reviewed profile's file, a fleet-scoped
/// one in the reserved `fleet.jsonl`; the reserved file is not reported as an
/// agent; describe() renders the line.
#[test]
fn test_check_line_roundtrip_and_reserved_fleet_file() {
    let dir = tmp_dir("check-line");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    // Agent-scoped check → the reviewed profile's file.
    log.log_check("coder", "agent", 120, 40, 2, 1_234);
    // Fleet-scoped check → the reserved fleet.jsonl (FLEET_FILE_STEM).
    log.log_check(FLEET_FILE_STEM, "fleet", 500, 120, 5, 5_000);

    // Agent-scoped check lives in coder.jsonl and round-trips its fields.
    let coder = log.read_all("coder");
    assert_eq!(coder.len(), 1, "got: {coder:?}");
    match &coder[0] {
        MetricsLine::Check {
            agent,
            scope,
            tokens_in,
            tokens_out,
            suggestions,
            duration_ms,
            ..
        } => {
            assert_eq!(agent, "coder");
            assert_eq!(scope, "agent");
            assert_eq!(*tokens_in, 120);
            assert_eq!(*tokens_out, 40);
            assert_eq!(*suggestions, 2);
            assert_eq!(*duration_ms, 1_234);
        }
        other => panic!("expected check line, got {other:?}"),
    }

    // Fleet-scoped check lives in the reserved fleet.jsonl.
    let fleet = log.read_all(FLEET_FILE_STEM);
    assert_eq!(fleet.len(), 1, "got: {fleet:?}");
    assert!(matches!(
        &fleet[0],
        MetricsLine::Check { scope, .. } if scope == "fleet"
    ));

    // The reserved fleet file must not show up as an agent.
    let names = log.agent_names();
    assert_eq!(names, vec!["coder".to_string()], "got: {names:?}");

    // describe() renders the line for raw-line lists.
    let d = coder[0].describe();
    assert!(d.contains("2 suggestion"), "got: {d}");
    assert!(d.contains("120 tok in"), "got: {d}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1c: check lines aggregate in the per-agent summary and the
/// `loop_cost_since` helper (per-agent and fleet-wide) reports the loop's own
/// cost.
#[test]
fn test_check_summary_and_loop_cost() {
    let dir = tmp_dir("check-summary");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    // A run (not counted as loop cost) + two agent checks for "coder".
    log.log_run("coder", &rs(4, 1, 1), 5_000, RunOutcome::Verified, 10, 5, "run-1", "sess-1");
    log.log_check("coder", "agent", 100, 20, 2, 1_000);
    log.log_check("coder", "agent", 300, 60, 3, 2_000);
    // A fleet check + a check for a second agent.
    log.log_check(FLEET_FILE_STEM, "fleet", 500, 100, 5, 4_000);
    log.log_check("researcher", "agent", 50, 10, 1, 300);

    // Per-agent summary counts the checks (and still reports the run).
    let s = log.summary_since("coder", None);
    assert_eq!(s.runs, 1);
    assert_eq!(s.checks, 2);
    assert_eq!(s.check_tokens_in, 400);
    assert_eq!(s.check_tokens_out, 80);
    assert_eq!(s.check_suggestions, 5);

    // Per-agent loop cost (7-day window covers all the `now()`-stamped lines).
    let (c, ti, to, sg) =
        log.loop_cost_since(Some("coder"), Utc::now() - chrono::Duration::days(7));
    assert_eq!((c, ti, to, sg), (2, 400, 80, 5));

    // Fleet-wide loop cost sums every agent file + the reserved fleet file.
    let (c, ti, to, sg) =
        log.loop_cost_since(None, Utc::now() - chrono::Duration::days(7));
    assert_eq!((c, ti, to, sg), (4, 950, 190, 11), "got: ({c}, {ti}, {to}, {sg})");

    // A window in the future reports zero cost.
    let (c, ti, to, sg) =
        log.loop_cost_since(None, Utc::now() + chrono::Duration::days(1));
    assert_eq!((c, ti, to, sg), (0, 0, 0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1c: the `record_check` free hook (the writer the improver actually calls)
/// lands in the DEFAULT log, honoring the process-global test override — so a
/// scope "agent" check goes to the profile file and a scope "fleet" check to
/// `fleet.jsonl`.
#[test]
fn test_record_check_writes_default_log() {
    let _lock = metrics_override_lock();
    let override_dir = tmp_dir("record-check");
    let _ = std::fs::remove_dir_all(&override_dir);
    set_metrics_dir_for_testing(Some(override_dir.clone()));
    record_check("coder", "agent", 11, 3, 1, 500);
    record_check(FLEET_FILE_STEM, "fleet", 22, 6, 2, 700);
    let log = MetricsLog::default();
    let coder = log.read_all("coder");
    assert_eq!(coder.len(), 1, "got: {coder:?}");
    assert!(matches!(
        &coder[0],
        MetricsLine::Check { scope, .. } if scope == "agent"
    ));
    let fleet = log.read_all(FLEET_FILE_STEM);
    assert_eq!(fleet.len(), 1, "got: {fleet:?}");
    assert!(matches!(
        &fleet[0],
        MetricsLine::Check { scope, .. } if scope == "fleet"
    ));
    set_metrics_dir_for_testing(None);
    let _ = std::fs::remove_dir_all(&override_dir);
}

/// 2a: `MetricsLogReader::poll` returns exactly the appended lines — a
/// full load, then incremental appends (the UI pattern: one rescan, then
/// only new lines per poll).
#[test]
fn test_reader_poll_incremental() {
    let dir = tmp_dir("reader-incremental");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("coder.jsonl");

    let write = |line: &str| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    };
    let run_line = |attempts: u32| {
        format!(
            r#"{{"kind":"run","ts":"2026-09-01T00:00:0{attempts}.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":{attempts},"duration_ms":100,"outcome":"verified","tokens_in":0,"tokens_out":0}}"#
        )
    };
    write(&run_line(0));

    let mut r = MetricsLogReader::new();
    let initial = r.load_all(&path);
    assert_eq!(initial.len(), 1, "got: {initial:?}");

    // Unchanged file: poll returns nothing.
    assert!(r.poll(&path).is_empty(), "unchanged file must not yield lines");

    write(&run_line(1));
    let new = r.poll(&path);
    assert_eq!(new.len(), 1, "got: {new:?}");
    assert!(matches!(&new[0], MetricsLine::Run { verification_attempts: 1, .. }));

    write(&run_line(2));
    write(&run_line(3));
    let new = r.poll(&path);
    assert_eq!(new.len(), 2, "got: {new:?}");
    assert!(matches!(&new[0], MetricsLine::Run { verification_attempts: 2, .. }));
    assert!(matches!(&new[1], MetricsLine::Run { verification_attempts: 3, .. }));

    // Missing file (deleted): poll returns nothing, no panic.
    std::fs::remove_file(&path).unwrap();
    assert!(r.poll(&path).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 2a: a torn trailing line (no newline yet) is buffered by `poll` and
/// delivered on the next poll once the write completes — it is never
/// returned half-parsed and never lost.
#[test]
fn test_reader_buffers_torn_line() {
    let dir = tmp_dir("reader-torn");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("coder.jsonl");
    use std::io::Write;
    let mut f = std::fs::File::create(&path).unwrap();
    let line1 = r#"{"kind":"run","ts":"2026-09-01T00:00:00.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":1,"duration_ms":1,"outcome":"verified","tokens_in":0,"tokens_out":0}"#;
    writeln!(f, "{line1}").unwrap();
    // Second line with NO trailing newline yet (a write in progress).
    let line2 = r#"{"kind":"run","ts":"2026-09-01T00:00:01.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":2,"duration_ms":1,"outcome":"verified","tokens_in":0,"tokens_out":0}"#;
    let partial = &line2[..line2.len() / 2];
    f.write_all(partial.as_bytes()).unwrap();
    drop(f);

    let mut r = MetricsLogReader::new();
    let first = r.poll(&path);
    assert_eq!(first.len(), 1, "only the complete line; got: {first:?}");
    assert!(matches!(
        first[0],
        MetricsLine::Run { verification_attempts: 1, .. }
    ));

    // Complete the torn line.
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(line2[partial.len()..].as_bytes()).unwrap();
    writeln!(f).unwrap();
    drop(f);

    let second = r.poll(&path);
    assert_eq!(second.len(), 1, "got: {second:?}");
    assert!(matches!(
        second[0],
        MetricsLine::Run { verification_attempts: 2, .. }
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 2a: when the file shrinks (truncated/rotated), `poll` transparently
/// rescans from the beginning and returns the full new content.
#[test]
fn test_reader_truncate_rescans() {
    let dir = tmp_dir("reader-truncate");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("coder.jsonl");
    use std::io::Write;
    let line = |a: u32| {
        format!(
            r#"{{"kind":"run","ts":"2026-09-01T00:00:0{a}.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":{a},"duration_ms":1,"outcome":"verified","tokens_in":0,"tokens_out":0}}"#
        )
    };
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{}", line(0)).unwrap();
    writeln!(f, "{}", line(1)).unwrap();
    drop(f);

    let mut r = MetricsLogReader::new();
    assert_eq!(r.load_all(&path).len(), 2);

    // Truncate the file down to a single (different) line.
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "{}", line(2)).unwrap();
    drop(f);

    let rescanned = r.poll(&path);
    assert_eq!(rescanned.len(), 1, "got: {rescanned:?}");
    assert!(matches!(&rescanned[0], MetricsLine::Run { verification_attempts: 2, .. }));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 2a: `MetricsSummary::from_lines` (the egui cache path) computes exactly
/// what `summary_since(name, None)` (the file path) does on the same
/// fixture — one full read, two computations, one result.
#[test]
fn test_from_lines_matches_summary_since() {
    let dir = tmp_dir("from-lines");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);

    let mut stats = rs(0, 0, 0);
    stats.bump_tool("shell", true, 50);
    stats.bump_tool("read_file", false, 30);
    stats.bump_tool("read_file", true, 20);
    log.log_run("coder", &stats, 1200, RunOutcome::Verified, 10, 2, "r1", "s1");
    log.log_run("coder", &rs(1, 0, 1), 800, RunOutcome::GaveUp, 5, 1, "r2", "s1");
    log.log_feedback("coder", true);
    log.log_feedback("coder", false);
    log.log_eval("coder", "e1", true, Some(0.9), 250, 100, 50, "gpt-4o-mini", 0.001);
    log.log_eval("coder", "e2", false, None, 300, 120, 60, "", 0.0);

    let lines = log.read_all("coder");
    let via_lines = MetricsSummary::from_lines(&lines);
    let via_file = log.summary_since("coder", None);
    assert_eq!(via_lines, via_file, "from_lines must equal summary_since(all time)");
    // And that the aggregate actually reflects the fixture (guards against
    // both paths silently emptying).
    assert_eq!(via_file.runs, 2);
    assert_eq!(via_file.tool_calls, 4);
    assert_eq!(via_file.tool_errors, 2);
    assert_eq!(via_file.verified, 1);
    assert_eq!(via_file.gave_up, 1);
    assert_eq!(via_file.feedback_up, 1);
    assert_eq!(via_file.feedback_down, 1);
    assert_eq!(via_file.evals, 2);
    assert_eq!(via_file.evals_passed, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2b test helper: local wall-clock datetime for a deterministic test
/// (port of the usage-stats helper — avoids DST-ambiguous local times).
fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Utc> {
    let nd = chrono::NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, 0)
        .unwrap();
    chrono::Local
        .from_local_datetime(&nd)
        .single()
        .expect("test time must not be ambiguous/nonexistent")
        .into()
}

/// 2b test helper: write one raw run line with a fixed ts (log_run stamps
/// Utc::now, which is not deterministic enough for window math).
fn write_run_line(path: &std::path::Path, ts: &str, attempts: u32, outcome: &str, duration: u64) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(
        f,
        r#"{{"kind":"run","ts":"{ts}","tool_calls":2,"tool_errors":1,"verification_attempts":{attempts},"duration_ms":{duration},"outcome":"{outcome}","tokens_in":10,"tokens_out":2}}"#
    )
    .unwrap();
}

/// 2b: `bucket_summary` returns a zero-filled window — the shared bucket
/// math on a sparse metrics fixture (mirror of the usage bucket tests).
#[test]
fn test_bucket_summary_zero_filled_day_window() {
    let dir = tmp_dir("bucket-day");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");

    let now = local(2026, 9, 15, 12, 0); // local noon on a Tuesday
    let today = local(2026, 9, 15, 9, 0).to_rfc3339();
    let five_days_ago = local(2026, 9, 10, 9, 0).to_rfc3339();
    write_run_line(&path, &five_days_ago, 2, "gave_up", 500);
    write_run_line(&path, &today, 1, "verified", 700);

    let buckets = log.bucket_summary("coder", Granularity::Day, now);
    assert_eq!(buckets.len(), 30, "day window is always 30 buckets");

    // Window = Aug 17 .. Sep 15 (local). Index = days since Aug 17.
    assert_eq!(
        buckets[24].start.date(),
        chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
        "bucket 24 is Sep 10"
    );
    assert_eq!(buckets[24].runs, 1);
    assert_eq!(buckets[24].tool_errors, 1);
    assert_eq!(buckets[24].gave_up, 1);
    assert_eq!(buckets[24].duration_ms_sum, 500);
    assert_eq!(buckets[24].tokens_in, 10);

    assert_eq!(
        buckets[29].start.date(),
        chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
        "bucket 29 is Sep 15"
    );
    assert_eq!(buckets[29].runs, 1);
    assert_eq!(buckets[29].verified_after_retry, 0);
    assert_eq!(buckets[29].duration_ms_sum, 700);

    // Every other bucket is zero-filled.
    let total_runs: u32 = buckets.iter().map(|b| b.runs).sum();
    assert_eq!(total_runs, 2, "exactly the two fixture runs");
    let nonzero = buckets
        .iter()
        .enumerate()
        .filter(|(_, b)| b.runs != 0)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert_eq!(nonzero, vec![24, 29]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2b: ported from the usage DST test — entries one wall-clock hour apart
/// across local midnight (near the US DST fall-back) land in ADJACENT
/// hour buckets; wall-clock bucket math is immune to the fold.
#[test]
fn test_bucket_summary_hours_adjacent_across_midnight() {
    let dir = tmp_dir("bucket-dst");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");

    let now = local(2026, 11, 1, 1, 0); // early local, near US DST fall-back
    let a = local(2026, 10, 31, 23, 15);
    let b = local(2026, 11, 1, 0, 45);
    write_run_line(&path, &a.to_rfc3339(), 1, "verified", 1);
    write_run_line(&path, &b.to_rfc3339(), 1, "verified", 1);

    let buckets = log.bucket_summary("coder", Granularity::Hour, now);
    assert_eq!(buckets.len(), 24);
    // Window = [02:00 Oct 31 .. 01:00 Nov 1] local: a -> bucket 21 (23:00),
    // b -> bucket 22 (00:00) — adjacent wall-clock hours, exactly one apart.
    let filled: Vec<usize> = buckets
        .iter()
        .enumerate()
        .filter(|(_, b)| b.runs != 0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(filled, vec![21, 22], "runs must sit in adjacent hour buckets");
    assert_eq!(
        buckets[22].start.time(),
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        "bucket 22 starts at local midnight"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2c: `percentile` — linear interpolation, edges, clamping.
#[test]
fn test_percentile_table() {
    assert_eq!(percentile(&[], 50.0), None, "empty -> None");
    assert_eq!(percentile(&[7.0], 50.0), Some(7.0), "single element -> itself");
    assert_eq!(percentile(&[7.0], 100.0), Some(7.0));
    assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 50.0), Some(2.5), "odd midpoint");
    assert_eq!(percentile(&[1.0, 2.0, 3.0], 50.0), Some(2.0), "even midpoint = middle");
    assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.0), Some(1.0), "p0 = min");
    assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 100.0), Some(4.0), "p100 = max");
    assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 150.0), Some(4.0), "clamped high");
    assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], -10.0), Some(1.0), "clamped low");
    assert_eq!(percentile(&[1.0, 2.0], f64::NAN), None, "non-finite p -> None");
    assert_eq!(percentile(&[10.0, 20.0, 30.0, 40.0], 95.0), Some(38.5));
}

/// 2c: `report` — percentiles, per-tool fold and model mix on a 3-run
/// fixture (raw lines with fixed ts, a tools histogram, and models).
#[test]
fn test_report_percentiles_tools_model_mix() {
    let dir = tmp_dir("report");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");
    use std::io::Write;
    let now = chrono::Utc::now();
    let line = |hours_ago: i64, body: &str| {
        let ts = (now - chrono::Duration::hours(hours_ago)).to_rfc3339();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, r#"{{"kind":"run","ts":"{ts}",{body}}}"#).unwrap();
    };
    line(
        1,
        r#""tool_calls":3,"tool_errors":1,"verification_attempts":1,"duration_ms":100,"outcome":"verified","tokens_in":10,"tokens_out":2,"tools":[{"name":"shell","calls":2,"errors":1,"duration_ms":300},{"name":"read_file","calls":1,"errors":0,"duration_ms":50}],"model":"m-a""#,
    );
    line(
        2,
        r#""tool_calls":1,"tool_errors":0,"verification_attempts":1,"duration_ms":300,"outcome":"verified","tokens_in":20,"tokens_out":20,"tools":[{"name":"shell","calls":1,"errors":0,"duration_ms":100}],"model":"m-a""#,
    );
    // NOTE: "model" comes BEFORE "tools" on purpose — a raw r#"..."# string
    // terminates at the first "#, so the line must not end with a quote.
    line(
        3,
        r#""tool_calls":0,"tool_errors":0,"verification_attempts":1,"duration_ms":900,"outcome":"gave_up","tokens_in":5,"tokens_out":5,"model":"","tools":[]"#,
    );

    let r = log.report("coder", None, None);
    assert_eq!(r.summary.runs, 3);
    assert_eq!(r.summary.gave_up, 1);
    // durations [100,300,900]: p50 = 300, p95 = 300 + 0.9*(900-300) = 840.
    assert_eq!(r.p50_duration_ms, Some(300));
    assert_eq!(r.p95_duration_ms, Some(840));
    // per-run tokens [12,40,10] -> sorted [10,12,40]: p50 = 12,
    // p95 = 12 + 0.9*28 = 37.2 -> 37.
    assert_eq!(r.p50_tokens, Some(12));
    assert_eq!(r.p95_tokens, Some(37));
    // Tool fold: shell 3 calls / 1 error / 400ms (avg 133), read_file 1/0/50.
    assert_eq!(
        r.tool_stats,
        vec![
            ReportToolStat {
                name: "read_file".to_string(),
                calls: 1,
                errors: 0,
                avg_ms: 50,
            },
            ReportToolStat {
                name: "shell".to_string(),
                calls: 3,
                errors: 1,
                avg_ms: 133,
            },
        ]
    );
    // Model mix: m-a x2 (most-used first), empty model -> "(unknown)" x1.
    assert_eq!(
        r.model_mix,
        vec![("m-a".to_string(), 2), ("(unknown)".to_string(), 1)]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2c: `compare` splits at the window boundary — runs inside
/// `[now-days, now)` count as current, the same-length preceding window
/// holds the rest (and both sides share the same reader computation).
#[test]
fn test_compare_window_split() {
    let dir = tmp_dir("compare");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");
    use std::io::Write;
    let now = chrono::Utc::now();
    let run_at = |hours_ago: i64| {
        let ts = (now - chrono::Duration::hours(hours_ago)).to_rfc3339();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            r#"{{"kind":"run","ts":"{ts}","tool_calls":1,"tool_errors":0,"verification_attempts":1,"duration_ms":10,"outcome":"verified","tokens_in":1,"tokens_out":1}}"#
        )
        .unwrap();
    };
    run_at(1); // current (1d window)
    run_at(20); // current
    run_at(40); // previous (window: 24h..48h ago)
    run_at(96); // outside both windows (4 days ago)

    let (cur, prev) = log.compare("coder", 1);
    assert_eq!(cur.summary.runs, 2, "current = [now-1d, now)");
    assert_eq!(prev.summary.runs, 1, "previous = [now-2d, now-1d)");
    // The report computation matches the same-window summary.
    assert_eq!(
        cur,
        log.report("coder", Some(now - chrono::Duration::hours(24)), Some(now))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 3a: `fleet_loop_status` unions the metrics store's agents with the
/// memory manager's per-agent loop states (loop-only agents still appear,
/// all-zero), aggregates fleet spend, and surfaces the loop config.
#[test]
fn test_fleet_loop_status_unions_and_sums() {
    let dir = tmp_dir("fleet-status");
    let _ = std::fs::remove_dir_all(&dir);
    let log = MetricsLog::new(&dir);
    log.log_run("coder", &rs(10, 1, 1), 1000, RunOutcome::Verified, 100, 20, "r1", "s1");

    let mdir = tmp_dir("fleet-status-mem");
    let _ = std::fs::remove_dir_all(&mdir);
    let config = crate::memory::MemoryConfig {
        memories_dir: Some(mdir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = crate::memory::MemoryManager::new(config).unwrap();
    memory.record_agent_improvement_check("coder", true);
    memory.record_agent_improvement_check("writer", false); // no metrics file

    let st = fleet_loop_status(&log, Some(&memory), 7);
    assert_eq!(st.window_days, 7);
    assert_eq!(st.agents.len(), 2, "union of metrics + loop agents");
    assert_eq!(st.agents[0].name, "coder");
    assert!(st.agents[0].loop_state.is_some());
    assert_eq!(st.agents[0].summary.runs, 1);
    assert!(st.agents[0].last_line.is_some(), "run in window");
    assert_eq!(st.agents[1].name, "writer");
    assert!(st.agents[1].loop_state.is_some());
    assert_eq!(st.agents[1].summary.runs, 0, "loop-only agent: empty window");
    assert!(st.agents[1].last_line.is_none());

    assert_eq!(st.fleet_spend.runs, 1);
    assert_eq!(st.fleet_spend.tokens_in, 100);
    assert_eq!(st.fleet_spend.tokens_out, 20);

    let cfg = st.loop_config.expect("memory manager wired");
    assert!(cfg.auto_improve, "default config has auto_improve on");
    assert_eq!(cfg.lessons, 0);

    // No memory manager: metrics only, no loop rows.
    let st = fleet_loop_status(&log, None, 7);
    assert!(st.loop_config.is_none());
    assert_eq!(st.agents.len(), 1);
    assert!(st.agents[0].loop_state.is_none());

    // last_activity: the agent's newest line (any kind); None when absent.
    assert!(log.last_activity("coder").is_some());
    assert!(log.last_activity("writer").is_none());

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&mdir);
}

/// 3b: the pure `metrics_report_from_lines` must produce EXACTLY the same
/// report as `MetricsLog::report` over the same file (the egui agent editor
/// feeds the cached vec to the pure fn; both paths must agree).
#[test]
fn test_report_from_lines_matches_report() {
    let dir = tmp_dir("report-from-lines");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");
    use std::io::Write;
    // One run with a tool histogram + model + cost, one plain run, one
    // feedback line (non-Run lines must not count as runs).
    {
        let mut f = OpenOptions::new().create(true).append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"kind":"run","ts":"2026-09-10T00:00:00.000Z","tool_calls":3,"tool_errors":1,"verification_attempts":1,"duration_ms":5000,"outcome":"verified","tokens_in":100,"tokens_out":50,"tools":[{{"name":"shell","calls":2,"errors":1,"duration_ms":3000}},{{"name":"read_file","calls":1,"errors":0,"duration_ms":100}}],"model":"test-model","cost_usd":0.05}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"kind":"run","ts":"2026-09-12T00:00:00.000Z","tool_calls":1,"tool_errors":0,"verification_attempts":1,"duration_ms":2000,"outcome":"gave_up","tokens_in":10,"tokens_out":5}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"kind":"feedback","ts":"2026-09-12T01:00:00.000Z","feedback":"up"}}"#
        )
        .unwrap();
    }

    let lines = log.read_all("coder");
    let via_method = log.report("coder", None, None);
    let via_lines = metrics_report_from_lines(&lines, None, None);
    assert_eq!(via_method, via_lines, "pure fn and method must agree");
    // Spot-check the interesting bits actually landed (not both empty).
    assert_eq!(via_lines.summary.runs, 2);
    assert_eq!(via_lines.summary.tool_calls, 4);
    assert_eq!(via_lines.cost_usd, 0.05);
    assert_eq!(via_lines.tool_stats.len(), 2);
    let shell = via_lines.tool_stats.iter().find(|t| t.name == "shell").unwrap();
    assert_eq!((shell.calls, shell.errors, shell.avg_ms), (2, 1, 1500));
    // Tie (1 run each) → name-ascending: "(unknown)" sorts before "test-model".
    assert_eq!(via_lines.model_mix, vec![("(unknown)".to_string(), 1), ("test-model".to_string(), 1)]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 3b: `metrics_report_from_lines` windows by `since`/`end` the same way
/// `report` does (the egui 7d/30d/all-time toggle feeds this).
#[test]
fn test_report_from_lines_window() {
    let dir = tmp_dir("report-window");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");
    use std::io::Write;
    let now = Utc::now();
    let run_at = |hours_ago: i64, outcome: &str| {
        let ts = (now - chrono::Duration::hours(hours_ago)).to_rfc3339();
        let mut f = OpenOptions::new().create(true).append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"kind":"run","ts":"{ts}","tool_calls":1,"tool_errors":0,"verification_attempts":1,"duration_ms":10,"outcome":"{outcome}","tokens_in":1,"tokens_out":1}}"#
        )
        .unwrap();
        drop(f);
    };
    run_at(1, "verified"); // inside 7d AND 30d
    run_at(20, "verified"); // inside 7d AND 30d
    run_at(10 * 24, "gave_up"); // inside 30d only (10d ago)
    run_at(40 * 24, "verified"); // outside both (40d ago)

    let lines = log.read_all("coder");
    let all = metrics_report_from_lines(&lines, None, None);
    let w7 = metrics_report_from_lines(&lines, Some(now - chrono::Duration::days(7)), None);
    let w30 = metrics_report_from_lines(&lines, Some(now - chrono::Duration::days(30)), None);
    assert_eq!(all.summary.runs, 4);
    assert_eq!(w7.summary.runs, 2, "7d window: only the two fresh runs");
    assert_eq!(w7.summary.gave_up, 0);
    assert_eq!(w30.summary.runs, 3, "30d window: drops the 25d-old run");
    assert_eq!(w30.summary.gave_up, 1);
    // The windowed summary path must match the method over the same window.
    let via_method = log.report("coder", Some(now - chrono::Duration::days(30)), None);
    assert_eq!(w30.summary, via_method.summary);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 3b: `bucket_summary_from_lines` must match `MetricsLog::bucket_summary`
/// over the same file (the egui outcome mini-chart uses the pure fn).
#[test]
fn test_bucket_from_lines_matches_method() {
    let dir = tmp_dir("bucket-from-lines");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");
    use std::io::Write;
    let now = Utc::now();
    // Runs spread across the last 3 days (all inside the Day window).
    for h in [1, 30, 70] {
        let ts = (now - chrono::Duration::hours(h)).to_rfc3339();
        let mut f = OpenOptions::new().create(true).append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"kind":"run","ts":"{ts}","tool_calls":2,"tool_errors":1,"verification_attempts":1,"duration_ms":100,"outcome":"gave_up","tokens_in":7,"tokens_out":3}}"#
        )
        .unwrap();
        drop(f);
    }

    let lines = log.read_all("coder");
    let via_method = log.bucket_summary("coder", Granularity::Day, now);
    let via_lines = bucket_summary_from_lines(&lines, Granularity::Day, now);
    assert_eq!(via_method, via_lines, "pure fn and method must agree");
    assert_eq!(via_lines.len(), 30, "Day window is 30 buckets");
    let total: u32 = via_lines.iter().map(|b| b.runs).sum();
    assert_eq!(total, 3, "all three runs land in the window");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 3d: run-level feedback lines carry `run_id` through serde, legacy
/// message-level lines (no `run_id` field) parse as `None`, and the summary
/// counts run-level feedback separately from — while still including it in —
/// the feedback totals.
#[test]
fn test_feedback_run_level_counts_and_legacy() {
    let dir = tmp_dir("feedback-run-level");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let path = log.agent_path("coder");
    use std::io::Write;
    let ts = Utc::now().to_rfc3339();
    // Legacy pre-3d lines: message-level (no `run_id` field in the JSON).
    let mut f = OpenOptions::new().create(true).append(true).open(&path).unwrap();
    writeln!(f, r#"{{"kind":"feedback","ts":"{ts}","feedback":"up"}}"#).unwrap();
    writeln!(f, r#"{{"kind":"feedback","ts":"{ts}","feedback":"down"}}"#).unwrap();
    drop(f);
    // 3d writers: run-level (linked) + the legacy message-level writer.
    log.log_feedback_run("coder", "run-abc", true);
    log.log_feedback_run("coder", "run-abc", false);
    log.log_feedback("coder", true);

    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 5);
    assert!(
        matches!(&lines[0], MetricsLine::Feedback { run_id: None, .. }),
        "legacy line must parse message-level: {lines:?}"
    );
    let MetricsLine::Feedback { run_id, .. } = &lines[2] else {
        panic!("expected a feedback line, got {lines:?}");
    };
    assert_eq!(run_id.as_deref(), Some("run-abc"), "writer must link the run");
    let rt: MetricsLine =
        serde_json::from_str(&serde_json::to_string(&lines[2]).unwrap()).unwrap();
    assert_eq!(rt, lines[2], "serde round-trip must preserve run_id");
    // Totals include run-level; the run-level subset is counted separately.
    let s = MetricsSummary::from_lines(&lines);
    assert_eq!(s.feedback_up, 3, "legacy up + run up + message up");
    assert_eq!(s.feedback_down, 2, "legacy down + run down");
    assert_eq!(s.feedback_run_up, 1);
    assert_eq!(s.feedback_run_down, 1);
    assert!(
        lines[2].describe().contains("(run run-abc)"),
        "{}",
        lines[2].describe()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4a: the global usage-path override is shared across the test binary's
/// parallel tests — serialize the tests that set it (same pattern as the
/// improvement tests' MetricsDirGuard).
static USAGE_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct UsagePathGuard(std::sync::MutexGuard<'static, ()>, PathBuf);

impl UsagePathGuard {
    fn new() -> Self {
        let guard = USAGE_PATH_LOCK.lock().unwrap();
        let path = tmp_dir("usage-join").join("usage.jsonl");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let _ = std::fs::remove_file(&path);
        crate::usage::recorder::UsageRecorder::set_usage_path_for_testing(Some(path.clone()));
        Self(guard, path)
    }
}

impl Drop for UsagePathGuard {
    fn drop(&mut self) {
        crate::usage::recorder::UsageRecorder::set_usage_path_for_testing(None);
    }
}

/// 4a: `run_detail` joins one run across the metrics + usage stores by the
/// 1e run_id — the run line, its LLM rounds (usage entries with the same
/// run_id, in log order, other runs' rounds excluded), and its run-level
/// feedback. Unknown run ids yield `None`.
#[test]
fn test_run_detail_joins_stores() {
    let _guard = UsagePathGuard::new();
    let dir = tmp_dir("run-detail");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    log.log_run("coder", &rs(3, 1, 1), 5_000, RunOutcome::Verified, 10, 2, "r1", "sess-1");
    log.log_run("coder", &rs(1, 0, 0), 1_000, RunOutcome::GaveUp, 5, 1, "r2", "sess-2");
    log.log_feedback_run("coder", "r1", true);

    use std::io::Write;
    let entry = |run_id: &str, tokens: u32| crate::usage::recorder::UsageEntry {
        ts: Utc::now(),
        session_id: "sess".to_string(),
        agent: "coder".to_string(),
        model: "test-model".to_string(),
        prompt_tokens: tokens,
        completion_tokens: 0,
        total_tokens: tokens,
        tool_calls: 1,
        thinking_chars: 42,
        run_id: run_id.to_string(),
        v: 1,
    };
    let mut f = std::fs::File::create(_guard.1.as_path()).unwrap();
    writeln!(f, "{}", serde_json::to_string(&entry("r1", 100)).unwrap()).unwrap();
    writeln!(f, "{}", serde_json::to_string(&entry("other", 999)).unwrap()).unwrap();
    writeln!(f, "{}", serde_json::to_string(&entry("r1", 50)).unwrap()).unwrap();
    drop(f);

    let d = log.run_detail("coder", "r1").expect("r1 exists");
    assert!(matches!(&d.run, MetricsLine::Run { run_id, .. } if run_id == "r1"));
    assert_eq!(d.rounds.len(), 2, "only r1's rounds (log order): {d:?}");
    assert_eq!(d.rounds[0].total_tokens, 100);
    assert_eq!(d.rounds[1].total_tokens, 50);
    assert_eq!(d.total_round_tokens(), 150);
    assert_eq!(d.feedback.len(), 1, "the run-level feedback links in");
    assert!(
        matches!(&d.feedback[0], MetricsLine::Feedback { run_id: Some(r), .. } if r == "r1")
    );
    assert!(log.run_detail("coder", "nope").is_none(), "unknown id → None");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4a: the report's trim correlation joins Trim lines to Run lines by the
/// 1e run_id — trimmed vs untrimmed gave-up shares stay separate, and a
/// trim for a non-existent run id is ignored.
#[test]
fn test_report_trim_correlation() {
    let dir = tmp_dir("trim-corr");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    log.log_run("coder", &rs(1, 0, 1), 1_000, RunOutcome::GaveUp, 0, 0, "r1", "s");
    log.log_run("coder", &rs(1, 0, 1), 1_000, RunOutcome::Verified, 0, 0, "r2", "s");
    log.log_run("coder", &rs(1, 0, 1), 1_000, RunOutcome::GaveUp, 0, 0, "r3", "s");
    log.log_run("coder", &rs(1, 0, 1), 1_000, RunOutcome::Verified, 0, 0, "r4", "s");
    log.log_trim("coder", 100, 50, 1, false, false, "r1");
    log.log_trim("coder", 100, 50, 1, false, false, "r2");
    log.log_trim("coder", 100, 50, 1, false, false, "ghost"); // no such run

    let rep = log.report("coder", None, None);
    assert_eq!(
        rep.trim_correlation,
        TrimCorrelation {
            trimmed_runs: 2,
            trimmed_gave_up: 1,
            untrimmed_runs: 2,
            untrimmed_gave_up: 1,
        },
        "got {rep:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4a: `BucketSummary::retry_rate` is `verified_after_retry / runs` (0.0 for
/// an empty bucket).
#[test]
fn test_bucket_retry_rate() {
    use chrono::Local;
    let empty = BucketSummary {
        start: Local::now().naive_local(),
        runs: 0,
        tool_calls: 0,
        tool_errors: 0,
        gave_up: 0,
        verified_after_retry: 0,
        tokens_in: 0,
        tokens_out: 0,
        duration_ms_sum: 0,
    };
    assert_eq!(empty.retry_rate(), 0.0);
    let some = BucketSummary {
        runs: 4,
        verified_after_retry: 1,
        ..empty
    };
    assert!((some.retry_rate() - 0.25).abs() < f64::EPSILON);
}

// ── 4b: retention rollup + raw-line rotation ───────────────────────────────

/// 4b helper: a fixed "now" for the retention tests (UTC), so day/cutoff
/// arithmetic is deterministic.
fn rollup_now() -> DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-09-29T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// 4b helper: parse "YYYY-MM-DD" as a UTC midnight DateTime.
fn day_start(s: &str) -> DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(&format!("{s}T00:00:00Z"))
        .unwrap()
        .with_timezone(&Utc)
}

/// 4b helper: append a raw `run` JSONL line with an EXACT ts (the log_*
/// writers stamp `Utc::now()`, which retention tests can't rely on).
fn write_run_at(log: &MetricsLog, agent: &str, ts: &str, run_id: &str, outcome: &str) {
    let path = log.agent_path(agent);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        r#"{{"kind":"run","ts":"{ts}","tool_calls":2,"tool_errors":1,"verification_attempts":1,"duration_ms":1000,"outcome":"{outcome}","tokens_in":100,"tokens_out":10,"tools":[{{"name":"shell","calls":2,"errors":1,"duration_ms":300}}],"run_id":"{run_id}","session_id":"s","llm_ms":700,"tools_ms":300,"model":"m1","cost_usd":0.25}}"#
    )
    .unwrap();
}

/// 4b helper: the shared 4b fixture — two runs on 2026-06-01 (120 days
/// before `rollup_now`, beyond the 90-day retention), one run on
/// 2026-08-15 (inside retention), plus a trim (linked to the gave-up old
/// run) and a feedback from the old day.
fn rollup_fixture(name: &str) -> (std::path::PathBuf, MetricsLog) {
    let dir = tmp_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    write_run_at(&log, "coder", "2026-06-01T08:00:00Z", "r1", "verified");
    write_run_at(&log, "coder", "2026-06-01T09:00:00Z", "r2", "gave_up");
    write_run_at(&log, "coder", "2026-08-15T10:00:00Z", "r3", "verified");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log.agent_path("coder"))
        .unwrap();
    writeln!(
        f,
        r#"{{"kind":"trim","ts":"2026-06-01T09:05:00Z","chars_before":200000,"chars_after":100000,"messages_removed":40,"brief_updated":true,"run_id":"r2","overflow":false}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"kind":"feedback","ts":"2026-06-01T10:00:00Z","feedback":"up"}}"#
    )
    .unwrap();
    (dir, log)
}

/// 4b: a fully-elapsed day older than the retention gets rolled up and its
/// raw run lines pruned; the recent day and the high-signal non-run lines
/// (trim/feedback) survive untouched. The pass is idempotent.
#[test]
fn test_rollup_writes_and_prunes() {
    let (_dir, log) = rollup_fixture("rollup-basic");
    let written = log.roll_up_and_prune("coder", rollup_now(), 90);
    assert_eq!(written.len(), 1, "only the 120-day-old day is eligible");
    let day = chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
    assert_eq!(written[0], log.rollup_path("coder", day));

    let r: super::MetricsRollup =
        serde_json::from_str(&std::fs::read_to_string(&written[0]).unwrap()).unwrap();
    assert_eq!(r.day, "2026-06-01");
    assert_eq!(r.agent, "coder");
    assert_eq!(r.summary.runs, 2);
    assert_eq!(r.summary.gave_up, 1);
    assert!((r.cost_usd - 0.5).abs() < 1e-9, "cost sums over the day");
    // The day holds TWO runs, each with shell=(2,1,300) → the day total.
    assert_eq!(r.tools["shell"], (4, 2, 600));
    assert_eq!(r.model_mix["m1"], 2);
    // The trim line links to r2 (which gave up) — the correlation survives
    // the pruning of r2's run line.
    assert_eq!(r.trim_correlation.trimmed_runs, 1);
    assert_eq!(r.trim_correlation.trimmed_gave_up, 1);
    assert_eq!(r.trim_correlation.untrimmed_runs, 1);

    // Raw file: only r3's run line + the trim + the feedback remain.
    let lines = log.read_all("coder");
    let runs: Vec<_> = lines
        .iter()
        .filter(|l| matches!(l, MetricsLine::Run { .. }))
        .collect();
    assert_eq!(runs.len(), 1, "old day's run lines pruned, recent kept");
    assert!(
        lines.iter().any(|l| matches!(l, MetricsLine::Trim { .. })),
        "trim lines are never pruned"
    );
    assert!(
        lines.iter().any(|l| matches!(l, MetricsLine::Feedback { .. })),
        "feedback lines are never pruned"
    );

    // Idempotent: a second pass writes nothing and prunes nothing.
    assert!(log.roll_up_and_prune("coder", rollup_now(), 90).is_empty());
    assert_eq!(log.read_all("coder").len(), 3);
}

/// 4b: `report` and `summary_between` consult the rollups for pruned days,
/// so a retention rotation never changes what the dashboards (report) or
/// the I5 effect check (summary_between) read.
#[test]
fn test_report_merges_rollups() {
    let (_dir, log) = rollup_fixture("rollup-merge");
    log.roll_up_and_prune("coder", rollup_now(), 90);

    // All-time: 2 rolled + 1 raw.
    let all = log.report("coder", None, None);
    assert_eq!(all.summary.runs, 3);
    assert!((all.cost_usd - 0.75).abs() < 1e-9);
    // All-time shell: the rolled-up day (4,2) + the raw recent run (2,1).
    let shell = all.tool_stats.iter().find(|t| t.name == "shell").unwrap();
    assert_eq!((shell.calls, shell.errors), (6, 3));
    assert_eq!(
        all.model_mix.iter().find(|(m, _)| m == "m1").unwrap().1,
        3
    );
    assert_eq!(all.trim_correlation.trimmed_gave_up, 1);

    // A window covering ONLY the pruned day is served entirely from the
    // rollup (the raw file no longer has that day's lines).
    let old_win = log.report("coder", Some(day_start("2026-06-01")), Some(day_start("2026-06-02")));
    assert_eq!(old_win.summary.runs, 2, "rollup serves the pruned day");

    // A window covering only the raw day gets no rollup contribution.
    let new_win = log.report("coder", Some(day_start("2026-08-15")), Some(day_start("2026-08-16")));
    assert_eq!(new_win.summary.runs, 1);

    // summary_between (the I5 before/after path) merges the same way.
    assert_eq!(log.summary_between("coder", None, None).runs, 3);
    assert_eq!(
        log.summary_between("coder", Some(day_start("2026-06-01")), Some(day_start("2026-06-02")))
            .runs,
        2
    );
    assert_eq!(
        log.summary_between("coder", Some(day_start("2026-08-15")), Some(day_start("2026-08-16")))
            .runs,
        1
    );
}

/// 4b: a day whose runs straddle the retention cutoff is NOT rolled — the
/// cutoff must fall between whole days, so a partially-covered day keeps
/// its raw lines until a later pass (when it has fully elapsed).
#[test]
fn test_partial_day_not_pruned() {
    let dir = tmp_dir("rollup-partial");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    // Cutoff = now - 1d = 2026-09-28T12:00Z: yesterday's runs sit on both
    // sides of it; 09-27 is fully before it.
    write_run_at(&log, "coder", "2026-09-28T09:00:00Z", "a", "verified");
    write_run_at(&log, "coder", "2026-09-28T15:00:00Z", "b", "verified");
    write_run_at(&log, "coder", "2026-09-27T10:00:00Z", "c", "verified");

    let written = log.roll_up_and_prune("coder", rollup_now(), 1);
    assert_eq!(written.len(), 1, "only the fully-elapsed 09-27 rolls");
    assert_eq!(
        written[0],
        log.rollup_path("coder", chrono::NaiveDate::from_ymd_opt(2026, 9, 27).unwrap())
    );
    let lines = log.read_all("coder");
    let runs: Vec<_> = lines
        .iter()
        .filter(|l| matches!(l, MetricsLine::Run { .. }))
        .collect();
    assert_eq!(runs.len(), 2, "the straddling day's raw lines survive");
}

/// 4b: `maybe_daily_rollup` covers every agent and runs at most once per
/// UTC calendar day (the `.rollup-state` marker); the next day's pass is a
/// no-op when every rollup already exists.
#[test]
fn test_daily_rollup_marker() {
    let dir = tmp_dir("rollup-marker");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    write_run_at(&log, "coder", "2026-03-01T08:00:00Z", "x1", "verified");
    write_run_at(&log, "architect", "2026-03-01T09:00:00Z", "x2", "verified");

    assert_eq!(log.maybe_daily_rollup(rollup_now(), 90), 2, "both agents roll");
    assert_eq!(
        log.maybe_daily_rollup(rollup_now(), 90),
        0,
        "same-day pass is short-circuited by the marker"
    );
    let next_day = rollup_now() + chrono::Duration::days(1);
    assert_eq!(
        log.maybe_daily_rollup(next_day, 90),
        0,
        "next day re-runs but every rollup already exists"
    );
    // The marker file holds today's UTC date.
    let marker = std::fs::read_to_string(dir.join(".rollup-state")).unwrap();
    assert_eq!(marker.trim(), "2026-09-30");
}

/// 4d: the writers stamp `v: 1` — the migration anchor for future schema
/// changes. Round-trip: the real `log_run` writer writes, `read_all` reports
/// v == 1 (not the serde default).
#[test]
fn test_v_writer_stamps_one_and_roundtrips() {
    let dir = tmp_dir("v-stamp");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    let stats = rs(5, 1, 2);
    log.log_run("coder", &stats, 1_000, RunOutcome::Verified, 100, 200, "run-v", "sess-v");
    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 1);
    match &lines[0] {
        MetricsLine::Run { v, .. } => assert_eq!(*v, 1, "writer must stamp v:1"),
        other => panic!("expected a Run line, got: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4d: a line written BEFORE the `v` field existed (no `v` key in the JSON)
/// still parses — `#[serde(default)]` fills v == 0. Tolerant parsing of old
/// stores is unchanged by the schema-version migration.
#[test]
fn test_v_legacy_line_without_v_parses_as_zero() {
    let dir = tmp_dir("v-legacy");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = MetricsLog::new(&dir);
    // A pre-4d Run line: every field except `v`.
    let legacy = serde_json::to_string(&serde_json::json!({
        "kind": "run",
        "ts": "2026-09-01T10:00:00Z",
        "tool_calls": 4,
        "tool_errors": 0,
        "verification_attempts": 1,
        "duration_ms": 250,
        "outcome": "verified",
        "tokens_in": 10,
        "tokens_out": 20,
        "tools": [],
        "run_id": "legacy-run",
        "session_id": "legacy-sess",
        "llm_ms": 0,
        "tools_ms": 0,
        "model": "",
        "cost_usd": 0.0
    }))
    .unwrap();
    use std::io::Write;
    let mut f = std::fs::File::create(log.agent_path("coder")).unwrap();
    writeln!(f, "{legacy}").unwrap();
    drop(f);
    let lines = log.read_all("coder");
    assert_eq!(lines.len(), 1, "legacy line must parse");
    match &lines[0] {
        MetricsLine::Run { v, .. } => assert_eq!(*v, 0, "legacy line must default to v:0"),
        other => panic!("expected a Run line, got: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4d: the same tolerant-parse guarantee for the usage log's `UsageEntry` —
/// a pre-4d usage line (no `v`) loads with v == 0 via the incremental reader.
#[test]
fn test_v_legacy_usage_entry_defaults_zero() {
    let entry = crate::usage::recorder::UsageEntry {
        ts: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
        session_id: "u-legacy-sess".to_string(),
        agent: "coder".to_string(),
        model: String::new(),
        prompt_tokens: 12,
        completion_tokens: 34,
        total_tokens: 46,
        tool_calls: 1,
        thinking_chars: 0,
        run_id: "u-legacy".to_string(),
        v: 0,
    };
    // Serialize WITHOUT v to mimic a pre-4d line, then deserialize.
    let mut obj = serde_json::to_value(&entry).unwrap();
    obj.as_object_mut().unwrap().remove("v");
    let parsed: crate::usage::recorder::UsageEntry =
        serde_json::from_value(obj).expect("pre-4d usage line must parse");
    assert_eq!(parsed.v, 0, "legacy usage entry must default to v:0");
}
