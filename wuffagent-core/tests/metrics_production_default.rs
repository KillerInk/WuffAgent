//! Production-path check for the metrics default location.
//!
//! Unit tests run with the library's `cfg(test)` ON, so
//! [`MetricsLog::default()`] falls back to a per-process temp dir there
//! (test hygiene: production writers must not pollute the real
//! `~/.wuffagent/metrics` during `cargo test`).
//!
//! This INTEGRATION test is a separate crate linked against the library with
//! `cfg(test)` OFF — it covers the production branch: `default()` must
//! resolve to `~/.wuffagent/metrics` (same home as `usage.jsonl`), and the
//! append → read round-trip must work there.
//!
//! The round-trip test uses a dedicated agent name and cleans up after
//! itself, so the real metrics dir stays clean.

use wuffagent_core::agents::metrics::{MetricsLog, RunOutcome};

#[test]
fn production_default_resolves_to_wuffagent_home() {
    let dir = MetricsLog::default().dir().to_path_buf();
    assert_eq!(
        dir,
        wuffagent_core::config::get_wuffagent_home().join("metrics"),
        "production default() must target the real home, got: {:?}",
        dir
    );
    // Not the unit-test per-process temp dir.
    assert!(
        !dir.to_string_lossy().contains("wuffagent-metrics-test-"),
        "got: {:?}",
        dir
    );
}

#[test]
fn production_default_append_roundtrip() {
    let log = MetricsLog::default();
    let path = log.agent_path("integration-selftest");
    let _ = std::fs::remove_file(&path); // idempotent across runs
    log.log_run(
        "integration-selftest",
        &wuffagent_core::agents::types::RunStats {
            tool_calls: 1,
            tool_errors: 0,
            verification_attempts: 1,
            ..Default::default()
        },
        42,
        RunOutcome::Verified,
        12,
        4,
        "run-1",
        "sess-1",
    );
    log.log_feedback("integration-selftest", true);
    let lines = log.read_all("integration-selftest");
    assert_eq!(
        lines.len(),
        2,
        "expected the two appended lines in {:?}, got {lines:?}",
        path
    );
    let _ = std::fs::remove_file(&path); // keep the real metrics dir clean
}
