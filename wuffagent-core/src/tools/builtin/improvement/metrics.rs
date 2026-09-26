//! `read_metrics`: agent-readable access to the run-metrics store (2c).
//!
//! The metrics store (`crate::agents::metrics`) was write-only from the
//! agent's point of view: the improvement loop reads it, but the agent
//! itself (or a researcher handoff doing a regression analysis) had no way
//! to ask "how have my recent runs been?". This tool exposes a read-only
//! view: aggregates over a time window (runs, outcomes, error rate,
//! average/max duration, user feedback) plus the most recent raw lines.
//!
//! Two modes:
//! - `agent` given → that agent's window.
//! - `agent` omitted → fleet overview: one summary line per agent that has
//!   a metrics file, plus the skills used in the window (cross-agent file).
//!
//! Read-only and LLM-free: it just parses JSONL lines. Note the data model
//! has no per-tool error attribution (only per-run counts), so "worst
//! tool" is not available — the raw recent lines carry the per-run error
//! counts for manual digging.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use crate::agents::metrics::{MetricsLine, MetricsLog};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolSchema, ToolResult,
};

/// Fallback window when `days` is omitted for the standalone constructor.
/// 2e: the registered tool overrides this with config
/// `improvement_metrics_window_days` (see `with_default_days`).
const DEFAULT_DAYS: u64 = 7;
/// Hard ceiling for `days` (guards against an accidental whole-history dump
/// for chatty agents — the window's raw lines are capped too, but the
/// aggregates would still scan everything).
const MAX_DAYS: u64 = 30;
/// Cap on raw lines included in a response (newest first; the aggregates
/// cover the whole window regardless).
const MAX_RECENT_LINES: usize = 10;
/// When the window itself is empty, this many all-time lines are shown for
/// context (clearly labeled as outside the window).
const OUTSIDE_WINDOW_LINES: usize = 3;

/// Tool exposing the run-metrics store to agents (read-only).
pub struct ReadMetricsTool {
    /// Explicit log root (tests); `None` = `MetricsLog::default()`.
    log: Option<MetricsLog>,
    /// 2e: window (days) used when `days` is omitted — wired to config
    /// `improvement_metrics_window_days` at registration; `DEFAULT_DAYS` for
    /// the standalone constructor.
    default_days: u64,
}

impl ReadMetricsTool {
    pub fn new() -> Self {
        Self { log: None, default_days: DEFAULT_DAYS }
    }

    /// Use an explicit metrics root instead of the default location (tests).
    pub fn with_log(log: MetricsLog) -> Self {
        Self { log: Some(log), default_days: DEFAULT_DAYS }
    }

    /// 2e: override the default window (config `improvement_metrics_window_days`).
    pub fn with_default_days(mut self, days: u64) -> Self {
        self.default_days = days.max(1);
        self
    }

    /// `MetricsLog` for this call. Construction is side-effect free (no
    /// I/O), so rebuilding it per call is cheap and keeps the struct
    /// `Clone`-free.
    fn log(&self) -> MetricsLog {
        match &self.log {
            Some(l) => MetricsLog::new(l.dir().to_path_buf()),
            None => MetricsLog::default(),
        }
    }

    /// Agent-mode report: aggregates over the window + newest raw lines.
    fn agent_report(&self, agent: &str, days: u64) -> ToolOutput {
        let log = self.log();
        let all = log.read_all(agent);
        if all.is_empty() {
            return ToolOutput::success(format!(
                "No metrics recorded for '{agent}' (no file or empty — the agent may simply \
                 never have completed a run; see read_skill/list_improvement_status for the \
                 improvement side of the loop)."
            ));
        }
        let since = Utc::now() - Duration::days(days as i64);
        let window: Vec<&MetricsLine> = all.iter().filter(|l| line_ts(l) >= since).collect();

        if window.is_empty() {
            let recent: Vec<String> = all
                .iter()
                .rev()
                .take(OUTSIDE_WINDOW_LINES)
                .map(|l| l.describe())
                .collect();
            return ToolOutput::success(format!(
                "No metrics for '{agent}' in the last {days} day(s) \
                 ({} line(s) recorded overall).\nMost recent lines (OUTSIDE the window):\n{}",
                all.len(),
                recent.join("\n")
            ));
        }

        let summary = log.summary_between(agent, Some(since), None);
        let max_duration_ms = window
            .iter()
            .filter_map(|l| match l {
                MetricsLine::Run { duration_ms, .. } => Some(*duration_ms),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let recent: Vec<String> = window
            .iter()
            .rev()
            .take(MAX_RECENT_LINES)
            .map(|l| l.describe())
            .collect();

        ToolOutput::success(format!(
            "Metrics for '{agent}' — window: last {days} day(s) ({} line(s)):\n  {} run(s): \
             {} verified / {} verified_after_retry / {} gave_up / {} not verified\n  {} tool \
             call(s), {} errors ({:.1}% error rate)\n  duration: avg {:.1}s, max {:.1}s\n  \
             user feedback: {} up / {} down\nRecent lines (newest first, up to {} shown):\n{}",
            window.len(),
            summary.runs,
            summary.verified,
            summary.verified_after_retry,
            summary.gave_up,
            summary.not_verified,
            summary.tool_calls,
            summary.tool_errors,
            if summary.tool_calls > 0 {
                100.0 * summary.tool_errors as f64 / summary.tool_calls as f64
            } else {
                0.0
            },
            summary.avg_duration_secs(),
            max_duration_ms as f64 / 1000.0,
            summary.feedback_up,
            summary.feedback_down,
            MAX_RECENT_LINES,
            recent.join("\n  ")
        ))
    }

    /// Fleet-mode report: one summary line per agent + skills used.
    fn fleet_report(&self, days: u64) -> ToolOutput {
        let log = self.log();
        let names = log.agent_names();
        if names.is_empty() {
            return ToolOutput::success(
                "No agents have metrics files yet. Name a specific agent with `agent` to \
                 query one explicitly (the store is created on the first recorded run)."
                    .to_string(),
            );
        }
        let since = Utc::now() - Duration::days(days as i64);
        let mut lines = Vec::new();
        for name in names {
            let summary = log.summary_between(&name, Some(since), None);
            lines.push(match summary.format_labeled(&name) {
                s if s.is_empty() => format!("{name}: no activity in window"),
                s => s,
            });
        }
        let mut out = format!(
            "Fleet metrics — window: last {days} day(s):\n  {}",
            lines.join("\n  ")
        );
        let skills = log.skill_usage_since(Some(since));
        if !skills.is_empty() {
            out.push_str(&format!("\n  skills used in window: {}", skills.join(", ")));
        }
        ToolOutput::success(out)
    }
}

/// The timestamp of any metrics line (all variants carry one).
fn line_ts(line: &MetricsLine) -> chrono::DateTime<chrono::Utc> {
    match line {
        MetricsLine::Run { ts, .. }
        | MetricsLine::Feedback { ts, .. }
        | MetricsLine::SkillUse { ts, .. } => *ts,
    }
}

impl Tool for ReadMetricsTool {
    fn name(&self) -> &str {
        "read_metrics"
    }

    fn description(&self) -> &str {
        "Read the run-metrics store (per-agent run statistics written after every agent run): \
         aggregates over a time window (runs, verification outcomes, tool error rate, average/ \
         max duration, user feedback) plus the most recent raw lines. Params: agent (optional \
         profile name; omit for a fleet-wide one-line-per-agent overview), days (optional \
         window in days, default 7, max 30). Use it for regression analysis of your own (or \
         another agent's) performance."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_metrics".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "agent".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Agent profile name to read metrics for; omit for a \
                                          fleet-wide overview"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "days".to_string(),
                        FieldSchema {
                            type_name: "integer".to_string(),
                            description: format!(
                                "Window in days (default {}, max {MAX_DAYS})",
                                self.default_days
                            ),
                            nullable: true,
                        },
                    ),
                ])),
                required: vec![],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let default_days = self.default_days;
        let days = match params.get::<u64>("days") {
            None => default_days,
            Some(d) if (1..=MAX_DAYS).contains(&d) => d,
            Some(d) => {
                return Ok(ToolOutput::error(format!(
                    "days must be between 1 and {MAX_DAYS} (got {d})"
                )))
            }
        };
        let agent = params
            .get::<String>("agent")
            .filter(|a| !a.is_empty());
        Ok(match agent {
            Some(name) => self.agent_report(&name, days),
            None => self.fleet_report(days),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::metrics::RunOutcome;
    use crate::agents::metrics::FeedbackKind;

    fn tool_in(dir: &std::path::Path) -> ReadMetricsTool {
        ReadMetricsTool::with_log(MetricsLog::new(dir.to_path_buf()))
    }

    fn call(tool: &ReadMetricsTool, agent: Option<&str>, days: Option<u64>) -> ToolOutput {
        let mut values = HashMap::new();
        if let Some(a) = agent {
            values.insert("agent".to_string(), serde_json::json!(a));
        }
        if let Some(d) = days {
            values.insert("days".to_string(), serde_json::json!(d));
        }
        match tool.execute(ToolParams { values }) {
            Ok(out) => out,
            Err(e) => panic!("unexpected ToolError: {e}"),
        }
    }

    fn text(out: ToolOutput) -> String {
        match out {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        }
    }

    fn record_run(log: &MetricsLog, agent: &str, calls: u32, errors: u32, ms: u64) {
        log.log_run(agent, calls, errors, 1, ms, RunOutcome::Verified);
    }

    #[test]
    fn agent_mode_reports_window_aggregates_and_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 10, 2, 20_000);
        log.log_run("coder", 4, 0, 0, 60_000, RunOutcome::GaveUp);
        log.log_feedback("coder", true);
        log.log_feedback("coder", false);

        let tool = tool_in(dir.path());
        let out = text(call(&tool, Some("coder"), None));
        assert!(out.contains("Metrics for 'coder'"), "got: {out}");
        assert!(out.contains("2 run(s)"), "got: {out}");
        assert!(out.contains("1 verified"), "got: {out}");
        assert!(out.contains("1 gave_up"), "got: {out}");
        assert!(out.contains("14 tool call(s), 2 errors"), "got: {out}");
        assert!(out.contains("14.3% error rate"), "got: {out}");
        assert!(out.contains("avg 40.0s"), "got: {out}");
        assert!(out.contains("max 60.0s"), "got: {out}");
        assert!(out.contains("1 up / 1 down"), "got: {out}");
        assert!(out.contains("outcome: verified"), "got: {out}");
        assert!(out.contains("feedback: down"), "got: {out}");
    }

    #[test]
    fn agent_mode_empty_window_falls_back_to_outside_window_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        // Only an OLD line (30 days back) → the 1-day window is empty, so
        // the report falls back to context lines outside the window.
        log.append(
            "coder",
            &MetricsLine::Run {
                ts: Utc::now() - Duration::days(30),
                tool_calls: 3,
                tool_errors: 0,
                verification_attempts: 0,
                duration_ms: 5_000,
                outcome: RunOutcome::None,
            },
        );
        let tool = tool_in(dir.path());
        let out = text(call(&tool, Some("coder"), Some(1)));
        assert!(out.contains("No metrics for 'coder' in the last 1 day(s)"), "got: {out}");
        assert!(out.contains("1 line(s) recorded overall"), "got: {out}");
        assert!(out.contains("OUTSIDE the window"), "got: {out}");
    }

    #[test]
    fn unknown_agent_is_a_friendly_empty_answer() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let out = text(call(&tool, Some("ghost"), None));
        assert!(out.contains("No metrics recorded for 'ghost'"), "got: {out}");
    }

    #[test]
    fn fleet_mode_lists_every_agent_with_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 10, 1, 12_000);
        record_run(&log, "wuffagent", 2, 0, 8_000);
        log.log_skill_use("wuffagent-self-restart");

        let tool = tool_in(dir.path());
        let out = text(call(&tool, None, None));
        assert!(out.contains("Fleet metrics"), "got: {out}");
        assert!(out.contains("coder (1 run(s)"), "got: {out}");
        assert!(out.contains("wuffagent (1 run(s)"), "got: {out}");
        assert!(out.contains("skills used in window: wuffagent-self-restart"), "got: {out}");
    }

    #[test]
    fn fleet_mode_empty_store_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let out = text(call(&tool, None, None));
        assert!(out.contains("No agents have metrics files yet"), "got: {out}");
    }

    #[test]
    fn days_out_of_range_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        match call(&tool, None, Some(0)) {
            ToolOutput::Error(e) => assert!(e.contains("between 1 and 30"), "got: {e}"),
            other => panic!("expected error, got: {:?}", other),
        }
        match call(&tool, None, Some(MAX_DAYS + 1)) {
            ToolOutput::Error(e) => assert!(e.contains("between 1 and 30"), "got: {e}"),
            other => panic!("expected error, got: {:?}", other),
        }
    }

    #[test]
    fn schema_has_no_required_params() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let schema = tool.parameters_schema();
        assert_eq!(schema.name, "read_metrics");
        let input = schema.input_type.as_ref().unwrap();
        assert!(input.required.is_empty());
        let props = input.properties.as_ref().unwrap();
        assert!(props.contains_key("agent"));
        assert!(props.contains_key("days"));
    }

    /// 2e: `with_default_days` (wired to config `improvement_metrics_window_days`
    /// at registration) changes both the omitted-`days` window and the schema.
    #[test]
    fn with_default_days_changes_window_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 10, 2, 20_000); // now → inside any window

        let overridden = tool_in(dir.path()).with_default_days(2);
        // Omitted `days` now resolves to the overridden default (2, not 7).
        let out = text(call(&overridden, Some("coder"), None));
        assert!(out.contains("window: last 2 day(s)"), "got: {out}");
        // And the schema advertises the same default.
        let schema = overridden.parameters_schema();
        let days_desc = schema
            .input_type
            .as_ref()
            .unwrap()
            .properties
            .as_ref()
            .unwrap()
            .get("days")
            .unwrap()
            .description
            .clone();
        assert!(days_desc.contains("default 2"), "got: {days_desc}");

        // The standalone constructor still defaults to DEFAULT_DAYS (7).
        let std_tool = tool_in(dir.path());
        let out = text(call(&std_tool, Some("coder"), None));
        assert!(out.contains("window: last 7 day(s)"), "got: {out}");
    }

    /// The `FeedbackKind` import is exercised here so the test module's use
    /// list stays honest (log_feedback takes a bool).
    #[test]
    fn feedback_kinds_are_serialized_lowercase() {
        let up = serde_json::to_value(MetricsLine::Feedback {
            ts: Utc::now(),
            feedback: FeedbackKind::Up,
        })
        .unwrap();
        assert_eq!(up["kind"], "feedback");
    }
}
