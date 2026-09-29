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
//!   With `status: true` (fleet mode only) the overview becomes a
//!   LOOP-STATUS view: the improvement loop's per-agent state (last check,
//!   tasks since, no-op streak, last effect verdict) joined with each
//!   agent's windowed metrics + its most recent metric line, plus the
//!   fleet's token spend over the window (the loop-cost line).
//!
//! Read-only and LLM-free: it just parses JSONL lines. 2d: agent mode now
//! also renders the per-run distribution (`percentiles:`) and the
//! per-tool breakdown (`tools (top by errors):`, from the runs' tool
//! histograms written since 1a), and `compare: true` appends a
//! before/after window block (`Nd vs previous Nd` via the shared
//! `MetricsLog::compare` primitive).
//!
//! Layout (phase 4): the report renderers live in `report.rs`, the 4c file
//! export in `export.rs`; the tool struct, constructors and the `Tool` impl
//! stay here.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agents::metrics::MetricsLog;
use crate::memory::MemoryManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema, ToolResult,
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
    /// The memory manager (the improvement loop's per-agent state) — the
    /// fleet loop-status view (`status: true`) reads it; `None` (standalone
    /// constructor / tests) degrades to metrics-only.
    memory: Option<Arc<MemoryManager>>,
    /// 2e: window (days) used when `days` is omitted — wired to config
    /// `improvement_metrics_window_days` at registration; `DEFAULT_DAYS` for
    /// the standalone constructor.
    default_days: u64,
    /// 4c: export-directory override (test seam); `None` =
    /// `~/.wuffagent/exports`.
    export_dir: Option<std::path::PathBuf>,
}

impl ReadMetricsTool {
    pub fn new() -> Self {
        Self { log: None, memory: None, default_days: DEFAULT_DAYS, export_dir: None }
    }

    /// Use an explicit metrics root instead of the default location (tests).
    pub fn with_log(log: MetricsLog) -> Self {
        Self { log: Some(log), memory: None, default_days: DEFAULT_DAYS, export_dir: None }
    }

    /// 4c: override the export directory (tests — the default would write
    /// into the real `~/.wuffagent/exports`).
    pub fn with_export_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.export_dir = Some(dir);
        self
    }

    /// Wire the memory manager for the fleet loop-status view.
    pub fn with_memory(mut self, memory: Arc<MemoryManager>) -> Self {
        self.memory = Some(memory);
        self
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
         window in days, default 7, max 30), status (optional, fleet mode only: report the \
         improvement-LOOP state instead of the plain metrics overview — per-agent last check, \
         no-op streak/backoff, last effect verdict, most recent activity, and fleet token spend). \
         Use it for regression analysis of your own (or another agent's) performance, or with \
         status=true to see the self-improvement loop's fleet-wide state. Agent mode also \
         shows percentiles + a per-tool error table, and compare=true adds a before/after \
          window (Nd vs previous Nd). Agent mode with run_id drills into one run's cross-store \
          join view (its LLM rounds from the usage log + run-level feedback). export (optional, \"csv\" or \"json\") also writes the window's raw metric lines (all kinds) to a file under ~/.wuffagent/exports/ and appends the file path to the response (fleet mode adds an agent column)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_metrics".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "run_id".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Agent mode only: drill into ONE run's cross-store join \
                                          view (the run line, its LLM rounds from the usage log, \
                                          and its run-level feedback). Copy the id from a recent \
                                          run line of the same agent"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "compare".to_string(),
                        FieldSchema {
                            type_name: "boolean".to_string(),
                            description: "Agent mode only: append a before/after block \
                                          comparing the last `days` against the preceding \
                                          same-length window (runs, error rate, gave_up, \
                                          tokens, cost when priced)"
                                .to_string(),
                            nullable: true,
                        },
                    ),
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
                    (
                        "status".to_string(),
                        FieldSchema {
                            type_name: "boolean".to_string(),
                            description: "Fleet mode only (omit `agent`): report the \
                                          improvement-loop state (per-agent last check, no-op \
                                          streak, effect verdict, recent activity, token spend) \
                                          instead of the plain metrics overview"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "export".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Also write the window's raw metric lines (all kinds) to a file: \"csv\" (flattened with kind/ts columns, agent column in fleet mode) or \"json\" (array of raw lines). The file lands in ~/.wuffagent/exports/ and the response appends its path".to_string(),
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
        let run_id = params
            .get::<String>("run_id")
            .filter(|r| !r.is_empty());
        let status = params.get::<bool>("status").unwrap_or(false);
        let compare = params.get::<bool>("compare").unwrap_or(false);
        let export = params
            .get::<String>("export")
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase());
        if let Some(e) = &export {
            if e != "csv" && e != "json" {
                return Ok(ToolOutput::error(format!(
                    "export must be \"csv\" or \"json\" (got {e:?})"
                )));
            }
        }
        let out = match agent.clone() {
            // 4a: `run_id` drills into one run's cross-store join view.
            Some(name) if run_id.is_some() => self.run_detail_report(&name, &run_id.clone().unwrap()),
            // 2d: `compare` only changes the agent view (before/after block).
            Some(name) => self.agent_report(&name, days, compare),
            // `status` only changes the FLEET view; in agent mode the report
            // already shows the agent's own window in full.
            None if status => self.fleet_status_report(days),
            None => self.fleet_report(days),
        };
        // 4c: `export` also writes the window's raw lines (all kinds) to a
        // file and appends the path — the report text itself is unchanged.
        if let Some(kind) = export {
            let (n, path) = self
                .export_lines(agent.as_deref(), days, &kind)
                .map_err(ToolError::Execution)?;
            let text = match &out {
                ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
                ToolOutput::Error(e) => e.clone(),
            };
            return Ok(ToolOutput::success(format!(
                "{text}\n\nExported {n} metric line(s) (last {days} day(s)) to {}",
                path.display()
            )));
        }
        Ok(out)
    }
}


mod export;
mod report;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use crate::agents::metrics::{FeedbackKind, MetricsLine, RunOutcome};

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

    /// 4c test helper: like `call`, but also passes `export`.
    fn call_export(
        tool: &ReadMetricsTool,
        agent: Option<&str>,
        days: Option<u64>,
        export: &str,
    ) -> ToolOutput {
        let mut values = HashMap::new();
        if let Some(a) = agent {
            values.insert("agent".to_string(), serde_json::json!(a));
        }
        if let Some(d) = days {
            values.insert("days".to_string(), serde_json::json!(d));
        }
        values.insert("export".to_string(), serde_json::json!(export));
        match tool.execute(ToolParams { values }) {
            Ok(out) => out,
            Err(e) => panic!("unexpected ToolError: {e}"),
        }
    }

    /// 4c test helper: append one raw JSONL line with an EXACT ts (the log_*
    /// writers stamp now, which window tests can't rely on).
    fn append_raw_line(log: &MetricsLog, agent: &str, line: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log.agent_path(agent))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn call_status(tool: &ReadMetricsTool, days: Option<u64>) -> ToolOutput {
        let mut values = HashMap::new();
        values.insert("status".to_string(), serde_json::json!(true));
        if let Some(d) = days {
            values.insert("days".to_string(), serde_json::json!(d));
        }
        match tool.execute(ToolParams { values }) {
            Ok(out) => out,
            Err(e) => panic!("unexpected ToolError: {e}"),
        }
    }

    /// 1a test helper: a RunStats with scalar counters only (no histogram).
    fn run(calls: u32, errors: u32, attempts: u32) -> crate::agents::types::RunStats {
        crate::agents::types::RunStats {
            tool_calls: calls as usize,
            tool_errors: errors as usize,
            verification_attempts: attempts,
            ..Default::default()
        }
    }

    fn record_run(log: &MetricsLog, agent: &str, calls: u32, errors: u32, ms: u64) {
        log.log_run(agent, &run(calls, errors, 1), ms, RunOutcome::Verified, 0, 0, "run-1", "sess-1");
    }

    #[test]
    fn agent_mode_reports_window_aggregates_and_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 10, 2, 20_000);
        log.log_run("coder", &run(4, 0, 0), 60_000, RunOutcome::GaveUp, 0, 0, "run-1", "sess-1");
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

    /// 4c: token totals from per-run lines are aggregated in the summary and
    /// surfaced as cost evidence.
    #[test]
    fn agent_mode_aggregates_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        log.log_run("coder", &run(10, 2, 1), 20_000, RunOutcome::Verified, 100, 20, "run-1", "sess-1");
        log.log_run("coder", &run(4, 0, 0), 60_000, RunOutcome::GaveUp, 50, 10, "run-1", "sess-1");

        let tool = tool_in(dir.path());
        let out = text(call(&tool, Some("coder"), None));
        assert!(out.contains("tokens: 150 in / 30 out"), "got: {out}");
    }

    /// 2d: agent mode renders the per-run percentiles and the per-tool
    /// breakdown (top by errors, then by calls) from the runs' histograms.
    #[test]
    fn agent_mode_shows_percentiles_and_tool_table() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        let rs = crate::agents::types::RunStats {
            tool_calls: 5,
            tool_errors: 2,
            verification_attempts: 1,
            tools: vec![
                crate::agents::types::ToolStat {
                    name: "shell".into(),
                    calls: 3,
                    errors: 2,
                    duration_ms: 300,
                },
                crate::agents::types::ToolStat {
                    name: "read_file".into(),
                    calls: 2,
                    errors: 0,
                    duration_ms: 100,
                },
            ],
            ..Default::default()
        };
        log.log_run("coder", &rs, 40_000, RunOutcome::Verified, 10, 5, "run-1", "sess-1");
        log.log_run("coder", &run(0, 0, 0), 100_000, RunOutcome::GaveUp, 4, 2, "run-1", "sess-1");

        let tool = tool_in(dir.path());
        let out = text(call(&tool, Some("coder"), None));
        // durations [40s,100s]: p50 = 70s, p95 = 40 + 0.95*60 = 97s.
        assert!(
            out.contains("percentiles: p50 70.0s / p95 97.0s duration"),
            "got: {out}"
        );
        // per-run tokens [15,6] -> sorted [6,15]: p50 = 10.5 -> 11, p95 = 14.55 -> 15.
        assert!(out.contains("p50 11 / p95 15 tokens"), "got: {out}");
        // shell (2 errors) before read_file (0); shell avg 300/3 = 100ms.
        assert!(
            out.contains("tools (top by errors): shell: 3 calls, 66.7% err, avg 100ms"),
            "got: {out}"
        );
        assert!(
            out.contains("read_file: 2 calls, 0.0% err, avg 50ms"),
            "got: {out}"
        );
    }

    /// 2d: `compare: true` appends the before/after window block via the
    /// shared compare primitive; without the flag the block is absent.
    #[test]
    fn agent_mode_compare_renders_window_block() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 10, 2, 20_000); // current 7d window
        // One run 10 days ago -> previous 7d window (9/10 calls errored).
        use std::io::Write;
        let path = log.agent_path("coder");
        let old_ts = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            r#"{{"kind":"run","ts":"{old_ts}","tool_calls":10,"tool_errors":9,"verification_attempts":1,"duration_ms":20000,"outcome":"gave_up","tokens_in":0,"tokens_out":0}}"#
        )
        .unwrap();

        let tool = tool_in(dir.path());
        let mut values = HashMap::new();
        values.insert("agent".to_string(), serde_json::json!("coder"));
        values.insert("days".to_string(), serde_json::json!(7));
        values.insert("compare".to_string(), serde_json::json!(true));
        let out = text(match tool.execute(ToolParams { values }) {
            Ok(o) => o,
            Err(e) => panic!("unexpected ToolError: {e}"),
        });
        assert!(out.contains("window: 7d vs previous 7d"), "got: {out}");
        assert!(out.contains("runs 1 → 1"), "got: {out}");
        assert!(out.contains("err rate 90.0% → 20.0%"), "got: {out}");
        assert!(out.contains("gave_up 1 → 0"), "got: {out}");
        // Without compare: no window block.
        let plain = text(call(&tool, Some("coder"), Some(7)));
        assert!(!plain.contains("window: 7d vs previous"), "got: {plain}");
    }

    /// 2d: the schema exposes the new `compare` param (no required fields).
    #[test]
    fn schema_includes_compare_param() {
        let tool = tool_in(std::env::temp_dir().as_path());
        let schema = tool.parameters_schema();
        let props = schema
            .input_type
            .as_ref()
            .and_then(|j| j.properties.as_ref())
            .expect("object schema with properties");
        assert!(props.contains_key("compare"), "compare param missing");
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
            run_id: None,
            v: 1,
        })
        .unwrap();
        assert_eq!(up["kind"], "feedback");
    }

    /// Fleet loop status (`status: true`) joins the improvement loop's
    /// per-agent state (from the memory manager) with each agent's windowed
    /// metrics + its most recent line, plus the loop-cost line (fleet token
    /// spend over the window).
    #[test]
    fn fleet_status_view_joins_loop_state_and_metrics() {
        use crate::memory::{MemoryConfig, MemoryManager};

        let mem_dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(mem_dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let memory = std::sync::Arc::new(MemoryManager::new(config).unwrap());
        memory.record_agent_improvement_check("coder", false);
        memory.record_effect_verdict("coder", "regressed");

        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 10, 2, 20_000);
        log.log_run("coder", &run(4, 0, 0), 60_000, RunOutcome::GaveUp, 50, 10, "run-1", "sess-1");
        record_run(&log, "generalist", 3, 0, 5_000);

        let tool = tool_in(dir.path()).with_memory(memory);
        let out = text(call_status(&tool, None));
        assert!(out.contains("Loop status — window: last 7 day(s):"), "got: {out}");
        assert!(out.contains("loop: auto_improve on"), "got: {out}");
        assert!(out.contains("per-agent loop state"), "got: {out}");
        assert!(out.contains("verdict regressed"), "got: {out}");
        // metrics half: windowed aggregates + the agent's most recent line
        assert!(
            out.contains("per-agent metrics (window) + most recent line:"),
            "got: {out}"
        );
        assert!(
            out.contains("[last: ") && out.contains("outcome: gave_up"),
            "most recent line must render: {out}"
        );
        // generalist has metrics but no loop-state entry — still listed here
        assert!(out.contains("generalist (1 run(s)"), "got: {out}");
        // the loop-cost line: fleet token spend over the window + per-run avg
        assert!(out.contains("token spend (window): 50 in / 10 out"), "got: {out}");
        assert!(out.contains("over 3 run(s)"), "got: {out}");
        assert!(out.contains("per run"), "got: {out}");
    }

    /// No memory manager wired (standalone constructor): the view degrades to
    /// metrics only and says so.
    #[test]
    fn fleet_status_without_memory_degrades() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 1, 0, 1_000);

        let tool = tool_in(dir.path());
        let out = text(call_status(&tool, None));
        assert!(
            out.contains("loop state: (no memory manager wired — metrics only)"),
            "got: {out}"
        );
        assert!(out.contains("coder (1 run(s)"), "got: {out}");
    }

    /// `status` only changes the FLEET view — in agent mode it is ignored.
    #[test]
    fn status_flag_ignored_in_agent_mode() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 2, 0, 3_000);

        let tool = tool_in(dir.path());
        let plain = text(call(&tool, Some("coder"), None));
        let with_status = text({
            let mut values = HashMap::new();
            values.insert("agent".to_string(), serde_json::json!("coder"));
            values.insert("status".to_string(), serde_json::json!(true));
            match tool.execute(ToolParams { values }) {
                Ok(out) => out,
                Err(e) => panic!("unexpected ToolError: {e}"),
            }
        });
        assert_eq!(plain, with_status);
        assert!(!plain.contains("Loop status"), "got: {plain}");
    }

    /// 4a: `run_id` in agent mode drills into the cross-store join view —
    /// the run line, its usage-log rounds (other runs' rounds excluded), and
    /// its run-level feedback; unknown ids get an explanatory success.
    static RUN_DETAIL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct RunDetailGuard(std::sync::MutexGuard<'static, ()>);
    impl RunDetailGuard {
        fn new() -> Self {
            let guard = RUN_DETAIL_LOCK.lock().unwrap();
            let path = std::env::temp_dir()
                .join(format!("wuffagent-run-detail-usage-{}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            crate::usage::recorder::UsageRecorder::set_usage_path_for_testing(Some(path));
            Self(guard)
        }
    }
    impl Drop for RunDetailGuard {
        fn drop(&mut self) {
            crate::usage::recorder::UsageRecorder::set_usage_path_for_testing(None);
        }
    }

    #[test]
    fn run_id_drill_reports_rounds_and_feedback() {
        let _g = RunDetailGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 2, 0, 3_000);
        log.log_feedback_run("coder", "run-1", true);

        use std::io::Write;
        let entry = |run_id: &str, tokens: u32| crate::usage::recorder::UsageEntry {
            ts: chrono::Utc::now(),
            session_id: "s".to_string(),
            agent: "coder".to_string(),
            model: "m-1".to_string(),
            prompt_tokens: tokens,
            completion_tokens: 0,
            total_tokens: tokens,
            tool_calls: 1,
            thinking_chars: 7,
            run_id: run_id.to_string(),
            v: 1,
        };
        let upath = crate::usage::recorder::UsageRecorder::usage_log_path();
        let mut f = std::fs::File::create(&upath).unwrap();
        writeln!(f, "{}", serde_json::to_string(&entry("run-1", 120)).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&entry("run-2", 555)).unwrap()).unwrap();
        drop(f);

        let tool = tool_in(dir.path());
        let out = text({
            let mut values = HashMap::new();
            values.insert("agent".to_string(), serde_json::json!("coder"));
            values.insert("run_id".to_string(), serde_json::json!("run-1"));
            match tool.execute(ToolParams { values }) {
                Ok(out) => out,
                Err(e) => panic!("unexpected ToolError: {e}"),
            }
        });
        assert!(out.contains("Run detail for 'coder' (run run-1)"), "got: {out}");
        assert!(out.contains("LLM rounds (1): 120 total tokens"), "got: {out}");
        assert!(out.contains("m-1"), "got: {out}");
        assert!(!out.contains("555"), "other run's round must not leak in: {out}");
        assert!(out.contains("(run run-1)"), "run-level feedback: {out}");

        let out2 = text({
            let mut values = HashMap::new();
            values.insert("agent".to_string(), serde_json::json!("coder"));
            values.insert("run_id".to_string(), serde_json::json!("ghost"));
            match tool.execute(ToolParams { values }) {
                Ok(out) => out,
                Err(e) => panic!("unexpected ToolError: {e}"),
            }
        });
        assert!(out2.contains("No run with run_id 'ghost'"), "got: {out2}");
    }

    // ── 4c: export ────────────────────────────────────────────────────────

    /// 4c: `export=json` (agent mode) writes the windowed raw lines (all
    /// kinds, NO agent column) to the export dir, names the file
    /// `metrics-<agent>-<N>d-*.json`, and appends the path to the response.
    #[test]
    fn export_json_agent_mode_writes_window_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 2, 0, 3_000); // in-window run line
        log.log_feedback_run("coder", "run-1", true); // in-window feedback
        append_raw_line(
            &log,
            "coder",
            r#"{"kind":"feedback","ts":"2020-01-01T00:00:00.000Z","feedback":"down"}"#,
        ); // out-of-window

        let exports = dir.path().join("exports");
        let tool = tool_in(dir.path()).with_export_dir(exports.clone());
        let out = text(call_export(&tool, Some("coder"), Some(7), "json"));

        assert!(out.contains("Exported 2 metric line(s) (last 7 day(s)) to "), "got: {out}");
        let files: Vec<_> = std::fs::read_dir(&exports)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(files.len(), 1, "exactly one export file: {files:?}");
        assert!(
            files[0].starts_with("metrics-coder-7d-") && files[0].ends_with(".json"),
            "file name: {}",
            files[0]
        );
        let v: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(exports.join(&files[0])).unwrap(),
        )
        .unwrap();
        assert_eq!(v.len(), 2, "in-window lines only (the 2020 line is pruned)");
        let kinds: Vec<&str> = v
            .iter()
            .map(|x| x["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"run") && kinds.contains(&"feedback"), "{kinds:?}");
        assert!(
            v.iter().all(|x| x.get("agent").is_none()),
            "agent mode has no agent column: {v:?}"
        );
    }

    /// 4c: `export=csv` (fleet mode) flattens every file's windowed lines
    /// with a `kind`,`ts`,`agent` header prefix; the agent column carries the
    /// file stem and out-of-window lines are excluded.
    #[test]
    fn export_csv_fleet_mode_adds_agent_column() {
        let dir = tempfile::tempdir().unwrap();
        let log = MetricsLog::new(dir.path());
        record_run(&log, "coder", 2, 0, 3_000);
        log.log_feedback_run("architect", "run-a", true);
        append_raw_line(
            &log,
            "coder",
            r#"{"kind":"skill_use","ts":"2019-05-05T00:00:00.000Z","skill":"old-skill"}"#,
        );

        let exports = dir.path().join("exports");
        let tool = tool_in(dir.path()).with_export_dir(exports.clone());
        let out = text(call_export(&tool, None, Some(7), "csv"));
        assert!(out.contains("Exported 2 metric line(s)"), "got: {out}");

        let files: Vec<_> = std::fs::read_dir(&exports)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            files[0].starts_with("metrics-fleet-7d-") && files[0].ends_with(".csv"),
            "file name: {}",
            files[0]
        );
        let csv = std::fs::read_to_string(exports.join(&files[0])).unwrap();
        let mut lines = csv.lines();
        let header = lines.next().unwrap();
        assert!(
            header.starts_with("kind,ts,agent,"),
            "header has the fixed prefix: {header}"
        );
        let agent_col = header
            .split(',')
            .position(|h| h == "agent")
            .unwrap();
        let rows: Vec<Vec<&str>> = lines.map(|l| l.split(',').collect()).collect();
        assert_eq!(rows.len(), 2, "in-window lines only: {csv:?}");
        let mut agents: Vec<&str> = rows.iter().map(|r| r[agent_col]).collect();
        agents.sort();
        assert_eq!(agents, vec!["architect", "coder"], "agent column: {csv:?}");
        let mut kinds: Vec<&str> = rows.iter().map(|r| r[0]).collect();
        kinds.sort();
        assert_eq!(kinds, vec!["feedback", "run"], "kind column: {csv:?}");
    }

    /// 4c: an unknown `export` value is a friendly error (no file written).
    #[test]
    fn export_invalid_format_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("exports");
        let tool = tool_in(dir.path()).with_export_dir(exports.clone());
        match call_export(&tool, Some("coder"), Some(7), "xml") {
            ToolOutput::Error(e) => assert!(
                e.contains("export must be"),
                "friendly error names the valid values: {e}"
            ),
            other => panic!("expected an error, got: {other:?}"),
        }
        assert!(
            !exports.exists(),
            "no export dir is created for a rejected format"
        );
    }
}
