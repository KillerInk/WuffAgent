//! Tests for the `stats` aggregation module (moved out of stats.rs).

use super::*;
use crate::usage::recorder::UsageEntry;
use chrono::TimeZone;
use std::time::{SystemTime, UNIX_EPOCH};

/// Local wall-clock datetime for tests (deterministic regardless of the
/// machine's timezone; avoids DST-ambiguous 2:00–3:00 local times).
fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    let nd = NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, 0)
        .unwrap();
    Local
        .from_local_datetime(&nd)
        .single()
        .expect("test time must not be ambiguous/nonexistent")
        .into()
}

fn entry(ts: DateTime<Utc>, prompt: u32, completion: u32) -> UsageEntry {
    UsageEntry {
        ts,
        session_id: "s".to_string(),
        agent: "a".to_string(),
        model: "m".to_string(),
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        tool_calls: 0,
        thinking_chars: 0,
    }
}

fn tmp_path(name: &str) -> std::path::PathBuf {
    let nix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "wuffagent-usage-stats-{}-{}-{nix}",
        std::process::id(),
        name
    ))
}

// ── parse / load ──────────────────────────────────────────────────────

#[test]
fn parse_line_tolerates_junk() {
    assert!(parse_line("").is_none());
    assert!(parse_line("   ").is_none());
    assert!(parse_line("not json").is_none());
    // Valid JSON but wrong shape → also skipped, not a panic.
    assert!(parse_line(r#"{"nope":1}"#).is_none());
    let e = parse_line(
        r#"{"ts":"2026-09-21T12:00:00Z","session_id":"s","agent":"a","model":"m","prompt_tokens":1,"completion_tokens":2,"total_tokens":3}"#,
    );
    assert!(e.is_some());
    assert_eq!(e.unwrap().total_tokens, 3);
}

#[test]
fn load_entries_skips_bad_lines() {
    let path = tmp_path("load");
    let _ = std::fs::remove_file(&path);
    let good = r#"{"ts":"2026-09-21T12:00:00Z","session_id":"s","agent":"a","model":"m","prompt_tokens":1,"completion_tokens":2,"total_tokens":3}"#;
    std::fs::write(&path, format!("{good}\ngarbage\n\n{good}\n")).unwrap();
    let (entries, skipped) = load_entries(&path);
    assert_eq!(entries.len(), 2);
    assert_eq!(
        skipped, 1,
        "only the garbage line counts, not the blank one"
    );
    let _ = std::fs::remove_file(&path);
}

// ── hour bucketing ────────────────────────────────────────────────────

#[test]
fn hour_window_is_24_zero_filled_and_boundary_correct() {
    // "Now" is 15:00 local → window = the 24 wall-clock hours ending in
    // the current hour: [16:00 prev day .. 15:00].
    let now = local(2026, 9, 21, 15, 0);
    let before_window = local(2026, 9, 20, 15, 0); // exactly 1h before start — out
    let b0 = local(2026, 9, 20, 16, 30); // bucket 0 (16:00)
    let b23 = local(2026, 9, 21, 15, 10); // bucket 23 = current hour (15:00)
    let future = local(2026, 9, 21, 16, 0); // outside (future)
    let w = bucketize(
        &[
            entry(before_window, 10, 1),
            entry(b0, 100, 10),
            entry(b23, 200, 20),
            entry(future, 999, 99),
        ],
        Granularity::Hour,
        now,
    );
    assert_eq!(w.buckets.len(), 24);
    // Bucket 0 starts at 16:00 yesterday = now floored to the hour − 23h.
    let now_local = now.with_timezone(&Local).naive_local();
    assert_eq!(w.buckets[0].start, now_local - chrono::Duration::hours(23));
    assert_eq!(w.buckets[0].total_tokens, 110);
    assert_eq!(w.buckets[0].calls, 1);
    // Bucket 23 = the current hour (15:00).
    assert_eq!(w.buckets[23].total_tokens, 220);
    // Everything else zero.
    assert_eq!((1..23).map(|i| w.buckets[i].total_tokens).sum::<u64>(), 0);
    // Window totals exclude out-of-window entries.
    assert_eq!(w.total_tokens, 330);
    assert_eq!(w.calls, 2);
}

#[test]
fn hour_boundary_2330_vs_next_day_0010() {
    let now = local(2026, 9, 21, 23, 45);
    let a = local(2026, 9, 21, 23, 30); // bucket 23
    let b = local(2026, 9, 22, 0, 10); // future — outside the window
    let w = bucketize(&[entry(a, 5, 1), entry(b, 7, 1)], Granularity::Hour, now);
    assert_eq!(w.buckets[23].total_tokens, 6);
    assert_eq!(w.calls, 1);
}

// ── day bucketing ─────────────────────────────────────────────────────

#[test]
fn day_window_is_30_and_calendar_aligned() {
    let now = local(2026, 9, 21, 14, 0);
    let d0 = local(2026, 8, 23, 23, 59); // 30 days back — bucket 0
    let d1 = local(2026, 8, 23, 0, 1); // 30 days back — bucket 0
    let d_last = local(2026, 9, 21, 0, 30); // today — bucket 29
    let too_old = local(2026, 8, 22, 23, 0); // bucket -1 — excluded
    let w = bucketize(
        &[
            entry(d0, 1, 1),
            entry(d1, 2, 2),
            entry(d_last, 3, 3),
            entry(too_old, 100, 100),
        ],
        Granularity::Day,
        now,
    );
    assert_eq!(w.buckets.len(), 30);
    assert_eq!(w.buckets[0].total_tokens, 6);
    assert_eq!(w.buckets[0].calls, 2);
    assert_eq!(w.buckets[29].total_tokens, 6);
    assert_eq!(w.total_tokens, 12);
    // Day bucket starts are local midnights.
    assert_eq!(
        w.buckets[29].start.time(),
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    );
}

// ── week bucketing (Monday-based) ─────────────────────────────────────

#[test]
fn week_window_is_monday_based() {
    // 2026-09-21 is a Monday. Now = that Monday 14:00.
    let now = local(2026, 9, 21, 14, 0);
    let friday = local(2026, 9, 18, 10, 0); // previous week — bucket 10
    let sunday = local(2026, 9, 20, 23, 0); // previous week — bucket 10
    let monday = local(2026, 9, 21, 1, 0); // current week — bucket 11
    let next_week = local(2026, 10, 1, 12, 0); // week of 2026-09-28 — excluded
    let w = bucketize(
        &[
            entry(friday, 1, 1),
            entry(sunday, 2, 2),
            entry(monday, 4, 4),
            entry(next_week, 8, 8),
        ],
        Granularity::Week,
        now,
    );
    assert_eq!(w.buckets.len(), 12);
    // Friday and Sunday of the same ISO week share one bucket.
    assert_eq!(w.buckets[10].total_tokens, 6);
    assert_eq!(w.buckets[10].calls, 2);
    // Monday starts a NEW bucket.
    assert_eq!(w.buckets[11].total_tokens, 8);
    assert_eq!(w.buckets[11].calls, 1);
    assert_eq!(w.total_tokens, 14);
    // Week bucket starts are local Monday midnights.
    assert_eq!(
        w.buckets[11].start.date(),
        NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()
    );
    assert_eq!(
        w.buckets[11].start.time(),
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    );
}

// ── DST robustness ────────────────────────────────────────────────────
// Bucket math is pure wall-clock arithmetic, so a DST transition (a
// missing or repeated local hour) cannot shift boundaries: an entry in
// the 14:00 wall-clock hour always lands in the 14:00 bucket. The
// tests above already cross local midnight and a Monday boundary; this
// one pins that entries separated by exactly one wall-clock hour are
// always in adjacent buckets.
#[test]
fn wall_clock_hours_are_adjacent_across_midnight() {
    let now = local(2026, 11, 1, 1, 0); // early local, near US DST fall-back
    let a = local(2026, 10, 31, 23, 15);
    let b = local(2026, 11, 1, 0, 45);
    let w = bucketize(&[entry(a, 1, 0), entry(b, 1, 0)], Granularity::Hour, now);
    // Window = [02:00 Oct 31 .. 01:00 Nov 1]: a lands in bucket 21
    // (23:00), b in bucket 22 (00:00) — adjacent wall-clock hours.
    assert_eq!(w.buckets[21].total_tokens, 1);
    assert_eq!(w.buckets[22].total_tokens, 1);
}

// ── tool calls / thinking aggregation ─────────────────────────────────

#[test]
fn bucketize_aggregates_tool_calls_and_thinking() {
    let now = local(2026, 9, 21, 15, 0);
    let a = local(2026, 9, 21, 14, 10); // bucket 22
    let b = local(2026, 9, 21, 15, 10); // bucket 23
    let mut ea = entry(a, 100, 10);
    ea.tool_calls = 3;
    ea.thinking_chars = 1200;
    let mut eb = entry(b, 200, 20);
    eb.tool_calls = 1;
    eb.thinking_chars = 50;
    let w = bucketize(&[ea, eb], Granularity::Hour, now);
    assert_eq!(w.buckets[22].tool_calls, 3);
    assert_eq!(w.buckets[22].thinking_chars, 1200);
    assert_eq!(w.buckets[23].tool_calls, 1);
    assert_eq!(w.total_tool_calls, 4);
    assert_eq!(w.total_thinking_chars, 1250);
}

#[test]
fn parse_line_defaults_missing_tool_and_thinking_fields() {
    // Pre-existing lines (written before the fields existed) must parse
    // with zeroed tool_calls / thinking_chars.
    let e = parse_line(
        r#"{"ts":"2026-09-21T12:00:00Z","session_id":"s","agent":"a","model":"m","prompt_tokens":1,"completion_tokens":2,"total_tokens":3}"#,
    )
    .unwrap();
    assert_eq!(e.tool_calls, 0);
    assert_eq!(e.thinking_chars, 0);
    // …and lines that carry them round-trip.
    let e2 = parse_line(
        r#"{"ts":"2026-09-21T12:00:00Z","session_id":"s","agent":"a","model":"m","prompt_tokens":1,"completion_tokens":2,"total_tokens":3,"tool_calls":2,"thinking_chars":100}"#,
    )
    .unwrap();
    assert_eq!(e2.tool_calls, 2);
    assert_eq!(e2.thinking_chars, 100);
}

// ── incremental reader ────────────────────────────────────────────────

fn line(total: u32) -> String {
    format!(
        r#"{{"ts":"2026-09-21T12:{total:02}:00Z","session_id":"s","agent":"a","model":"m","prompt_tokens":{total},"completion_tokens":0,"total_tokens":{total}}}"#
    )
}

#[test]
fn reader_full_then_incremental() {
    let path = tmp_path("reader");
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, format!("{}\n", line(1))).unwrap();
    let mut r = UsageLogReader::new();
    let (entries, skipped) = r.load_all(&path);
    assert_eq!(entries.len(), 1);
    assert_eq!(skipped, 0);

    // No change → nothing new.
    assert_eq!(r.poll(&path), (Vec::new(), 0));

    // Append one good line + one bad line.
    use std::io::Write;
    {
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", line(2)).unwrap();
        writeln!(f, "garbage").unwrap();
    }
    let (entries, skipped) = r.poll(&path);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].total_tokens, 2);
    assert_eq!(skipped, 1);
    assert_eq!(r.total_skipped(), 1);

    // A partial trailing line (torn write) is buffered, not lost.
    {
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write as _;
        f.write_all(
            b"{\"ts\":\"2026-09-21T13:00:00Z\",\"session_id\":\"s\",\"agent\":\"a\",\"model\":\"m\",\"prompt_tokens\":3,\"completion_tokens\":0,\"total_tokens\":3",
        )
        .unwrap();
    }
    assert_eq!(r.poll(&path), (Vec::new(), 0));
    // Complete the line with the final newline.
    {
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write as _;
        f.write_all(b"}\n").unwrap();
    }
    let (entries, _) = r.poll(&path);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].total_tokens, 3);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn reader_full_rescan_when_file_shrinks() {
    let path = tmp_path("shrink");
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, format!("{}\n{}\n", line(1), line(2))).unwrap();
    let mut r = UsageLogReader::new();
    assert_eq!(r.load_all(&path).0.len(), 2);
    assert_eq!(r.poll(&path).0.len(), 0);

    // Rewrite the file with LESS content (truncate in place).
    std::fs::write(&path, format!("{}\n", line(9))).unwrap();
    let (entries, _) = r.poll(&path);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].total_tokens, 9);
    // And subsequent polls are incremental against the new size.
    assert_eq!(r.poll(&path).0.len(), 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn reader_missing_file_is_quiet() {
    let mut r = UsageLogReader::new();
    assert_eq!(r.poll(&tmp_path("missing")), (Vec::new(), 0));
    assert_eq!(r.load_all(&tmp_path("missing")).0.len(), 0);
}
