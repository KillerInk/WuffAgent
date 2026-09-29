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
