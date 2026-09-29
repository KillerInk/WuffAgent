//! M1: per-agent run metrics — append-only JSONL, one file per agent.
//!
//! Line kinds (`MetricsLine` variants, `kind` in JSON):
//! - `run` (M1): one per agent task run.
//! - `feedback` (3d): user thumbs up/down; `target` is `run:<run_id>` (per-run,
//!   recorded from the "this run" row) or `message` (per-message, recorded from the
//!   per-message row). Per-run feedback is also joinable by run_id.
//! - `skill_use` (2a): one per successful `read_skill` call (skill name).
//! - `trim` (2c/3c): one per context-trimming decision (kind + freed estimate).
//! - `check` (3a/4a): one per self-improvement check run (agent, produced,
//!   verdict — the verdict of the *previous* check, for 3a's effect check).
//! - `eval` (5c): one per headless golden-evaluation run (eval id, pass/fail,
//!   cost) — the basis for before/after regression checks after
//!   prompt/profile changes.
//!
//! File: `{config_dir}/metrics/<agent>.jsonl` — append-only, best-effort (a
//! failure to write metrics must never break the agent; the append is
//! fire-and-forget, errors go to debug logs). `read_all` skips blank and
//! corrupt lines (forward compatibility: older binaries ignore new kinds).
//! `MetricsLog::report`/`compare` are the report core (used by the 4b
//! read_metrics tool), `summary_since`/`bucket_summary` are the 2c aggregation
//! core (used by the UI). The 4b daily rollup (`roll_up_and_prune` /
//! `maybe_daily_rollup`) rewrites old days as per-day
//! `{agent}__rollup-YYYY-MM-DD.json` and prunes files older than
//! `retention_days`, so the hot file stays small and the disk is bounded.
//!
//! 3a: `fleet_loop_status` is the fleet loop status core (cost-control
//! config + per-agent loop state + token spend), shared by the
//! `read_metrics status=true` tool and the egui fleet dashboard.
//!
//! Sub-modules (modularization phase 3):
//! - `schema` — the line schema (`MetricsLine`, `RunOutcome`, `FeedbackKind`, `percentile`)
//! - `aggregates` — summaries, reports, rollup totals, `RunDetail` (pure line computation)
//! - `log` — the `MetricsLog` store (append/read/rollup) + the `record_*` shorthands
//! - `fleet` — the 3a fleet loop status
//! - `reader` — the incremental (byte-offset) reader for live UI polling
//! - `tests` — the M1 test suite

mod aggregates;
mod fleet;
mod log;
mod reader;
mod schema;

pub use aggregates::{
    agent_file_name, bucket_summary_from_lines, metrics_report_from_lines, BucketSummary,
    MetricsReport, MetricsRollup, MetricsSummary, ReportToolStat, RunDetail, TrimCorrelation,
};
pub use fleet::{
    fleet_loop_status, FleetAgentStatus, FleetLoopStatus, FleetSpend, LoopConfigInfo,
};
pub use log::{
    record_check, record_feedback, record_run, record_skill_use, record_trim,
    set_metrics_dir_for_testing, FLEET_FILE_STEM, MetricsLog, SKILLS_FILE_STEM,
};
pub use reader::MetricsLogReader;
pub use schema::{percentile, FeedbackKind, MetricsLine, RunOutcome};

// Re-exported (test-only) so the `tests` child module can reach it via
// `super::METRICS_DIR_LOCK`.
#[cfg(test)]
pub(crate) use log::METRICS_DIR_LOCK;

#[cfg(test)]
mod tests;
