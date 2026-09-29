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

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use crate::agents::metrics::{fleet_loop_status, MetricsLine, MetricsLog};
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

    /// Agent-mode report: aggregates over the window + newest raw lines.
    fn agent_report(&self, agent: &str, days: u64, compare: bool) -> ToolOutput {
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

        let report = log.report(agent, Some(since), None);
        let summary = report.summary;
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

        let err_rate = if summary.tool_calls > 0 {
            100.0 * summary.tool_errors as f64 / summary.tool_calls as f64
        } else {
            0.0
        };
        let mut out = format!(
            "Metrics for '{agent}' — window: last {days} day(s) ({} line(s)):\
             \n  {} run(s): \
             {} verified / {} verified_after_retry / {} gave_up / {} not verified\
             \n  {} tool \
             call(s), {} errors ({:.1}% error rate)\
             \n  duration: avg {:.1}s, max {:.1}s\
             \n  tokens: {} in / {} out\
             \n  user feedback: {} up / {} down",
            window.len(),
            summary.runs,
            summary.verified,
            summary.verified_after_retry,
            summary.gave_up,
            summary.not_verified,
            summary.tool_calls,
            summary.tool_errors,
            err_rate,
            summary.avg_duration_secs(),
            max_duration_ms as f64 / 1000.0,
            summary.tokens_in,
            summary.tokens_out,
            summary.feedback_up,
            summary.feedback_down,
        );

        // 2d: the per-run distribution — with a handful of long runs the
        // mean is misleading, so p50/p95 sit right next to it.
        let dur = |v: Option<u64>| match v {
            Some(ms) => format!("{:.1}s", ms as f64 / 1000.0),
            None => "-".to_string(),
        };
        let toks = |v: Option<u64>| v.map(|t| t.to_string()).unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "\n  percentiles: p50 {} / p95 {} duration; p50 {} / p95 {} tokens",
            dur(report.p50_duration_ms),
            dur(report.p95_duration_ms),
            toks(report.p50_tokens),
            toks(report.p95_tokens)
        ));

        // 4a: retry rate + trim correlation (derived from existing fields,
        // per the stats plan's cheap-correlation list).
        if summary.runs > 0 {
            let retry = 100.0 * summary.verified_after_retry as f64 / summary.runs as f64;
            out.push_str(&format!("\n  retry rate: {:.0}% of runs needed a nudge", retry));
        }
        let tc = report.trim_correlation;
        if tc.trimmed_runs > 0 || tc.untrimmed_runs > 0 {
            let share = |runs: u32, up: u32| {
                if runs > 0 {
                    100.0 * up as f64 / runs as f64
                } else {
                    0.0
                }
            };
            out.push_str(&format!(
                "\n  trim correlation: {} trimmed run(s) → {:.0}% gave_up | {} untrimmed → {:.0}% gave_up",
                tc.trimmed_runs,
                share(tc.trimmed_runs, tc.trimmed_gave_up),
                tc.untrimmed_runs,
                share(tc.untrimmed_runs, tc.untrimmed_gave_up)
            ));
        }

        // 2d: per-tool breakdown (top-5 by errors, then by calls) — "which
        // tool is most error-prone / slowest" is now answerable.
        if !report.tool_stats.is_empty() {
            let mut by_errors: Vec<&crate::agents::metrics::ReportToolStat> =
                report.tool_stats.iter().collect();
            by_errors.sort_by(|a, b| {
                b.errors
                    .cmp(&a.errors)
                    .then_with(|| b.calls.cmp(&a.calls))
                    .then_with(|| a.name.cmp(&b.name))
            });
            let top: Vec<String> = by_errors
                .into_iter()
                .take(5)
                .map(|t| {
                    let err = if t.calls > 0 {
                        100.0 * t.errors as f64 / t.calls as f64
                    } else {
                        0.0
                    };
                    format!(
                        "{}: {} calls, {:.1}% err, avg {}ms",
                        t.name, t.calls, err, t.avg_ms
                    )
                })
                .collect();
            out.push_str(&format!("\n  tools (top by errors): {}", top.join(" | ")));
        }

        // 2d: before/after window via the shared compare primitive.
        if compare {
            let (cur, prev) = log.compare(agent, days as u32);
            let rate = |s: &crate::agents::metrics::MetricsSummary| {
                if s.tool_calls > 0 {
                    100.0 * s.tool_errors as f64 / s.tool_calls as f64
                } else {
                    0.0
                }
            };
            let delta_pp = rate(&cur.summary) - rate(&prev.summary);
            out.push_str(&format!(
                "\n  window: {days}d vs previous {days}d: runs {} → {} | err rate {:.1}% → {:.1}% (Δ {:+.1}pp) | gave_up {} → {}",
                prev.summary.runs,
                cur.summary.runs,
                rate(&prev.summary),
                rate(&cur.summary),
                delta_pp,
                prev.summary.gave_up,
                cur.summary.gave_up
            ));
            out.push_str(&format!(
                "\n    tokens: {} in / {} out → {} in / {} out",
                prev.summary.tokens_in,
                prev.summary.tokens_out,
                cur.summary.tokens_in,
                cur.summary.tokens_out
            ));
            if prev.cost_usd > 0.0 || cur.cost_usd > 0.0 {
                out.push_str(&format!(
                    " | cost ${:.4} → ${:.4}",
                    prev.cost_usd, cur.cost_usd
                ));
            }
        }

        out.push_str(&format!(
            "\nRecent lines (newest first, up to {} shown):\n{}",
            MAX_RECENT_LINES,
            recent.join("\n  ")
        ));
        ToolOutput::success(out)
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

    /// Fleet-mode loop status (`status: true`): the improvement loop's
    /// per-agent state (last check, tasks since, no-op streak, last effect
    /// verdict — from the memory manager's improvement state) joined with
    /// each agent's windowed metrics and its most recent metric line (what
    /// actually happened since), plus the fleet's token spend over the
    /// window (the loop-cost line: the run budget the loop improves on).
    fn fleet_status_report(&self, days: u64) -> ToolOutput {
        // 3a: the structured core is shared with the egui fleet dashboard —
        // this fn is now just its text renderer.
        let log = self.log();
        let st = fleet_loop_status(&log, self.memory.as_deref(), days);
        let mut out = format!("Loop status — window: last {days} day(s):");

        // The loop half: cost-control settings + per-agent loop state (the
        // memory manager is optional — standalone tests degrade to metrics
        // only).
        match &st.loop_config {
            Some(cfg) => {
                out.push_str(&format!(
                    "\n  loop: auto_improve {}; cooldown 1 check / {} task(s), min interval {}; \
                     lessons in store: {}",
                    if cfg.auto_improve { "on" } else { "off" },
                    cfg.cooldown_tasks,
                    super::status::min_interval_label(cfg.min_interval_hours),
                    cfg.lessons,
                ));
                out.push_str(&format!(
                    "\n  last check (global/legacy): {}",
                    super::status::format_ago(cfg.last_check)
                ));
                if st.agents.iter().all(|a| a.loop_state.is_none()) {
                    out.push_str("\n  per-agent loop state: none recorded yet");
                } else {
                    out.push_str(
                        "\n  per-agent loop state (last check · tasks since · no-op streak · verdict):",
                    );
                    for a in &st.agents {
                        if let Some(ls) = &a.loop_state {
                            out.push_str(&format!(
                                "\n    {}: last check {}; {} task(s) since; no-op streak {}; verdict {}",
                                a.name,
                                super::status::format_ago(ls.last_check),
                                ls.runs_since_check,
                                ls.no_op_streak,
                                ls.last_effect_verdict
                                    .clone()
                                    .unwrap_or_else(|| "-".to_string()),
                            ));
                        }
                    }
                }
            }
            None => out.push_str(
                "\n  loop state: (no memory manager wired — metrics only)",
            ),
        }

        // The metrics half: windowed per-agent summary + the agent's most
        // recent line in the window (what actually happened).
        if st.agents.is_empty() {
            out.push_str(
                "\n  per-agent metrics: no agents have metrics files yet (the store is \
                 created on the first recorded run)",
            );
        } else {
            out.push_str("\n  per-agent metrics (window) + most recent line:");
            for a in &st.agents {
                let label = match a.summary.format_labeled(&a.name) {
                    s if s.is_empty() => format!("{}: no activity in window", a.name),
                    s => s,
                };
                match &a.last_line {
                    Some(l) => out.push_str(&format!("\n    {label} [last: {l}]")),
                    None => out.push_str(&format!("\n    {label}")),
                }
            }
        }

        // The loop-cost line: fleet token spend over the window (from the
        // metrics store — the runs the loop improves, not the improvement
        // checks' own LLM calls).
        let spend = st.fleet_spend;
        if spend.runs > 0 || spend.tokens_in > 0 || spend.tokens_out > 0 {
            out.push_str(&format!(
                "\n  token spend (window): {} in / {} out over {} run(s)",
                spend.tokens_in, spend.tokens_out, spend.runs
            ));
            if spend.runs > 0 {
                out.push_str(&format!(
                    " (avg {:.0} in / {:.0} out per run)",
                    spend.tokens_in as f64 / spend.runs as f64,
                    spend.tokens_out as f64 / spend.runs as f64
                ));
            }
        }
        ToolOutput::success(out)
    }
}

/// The timestamp of any metrics line (all variants carry one).
fn line_ts(line: &MetricsLine) -> chrono::DateTime<chrono::Utc> {
    match line {
        MetricsLine::Run { ts, .. }
        | MetricsLine::Feedback { ts, .. }
        | MetricsLine::SkillUse { ts, .. }
        | MetricsLine::Trim { ts, .. }
        | MetricsLine::Check { ts, .. }
        | MetricsLine::Eval { ts, .. } => *ts,
    }
}

impl ReadMetricsTool {
    /// 4a: the cross-store join view of one run — its own metrics line, its
    /// LLM rounds (usage.jsonl entries carrying the same 1e run_id), and its
    /// run-level feedback (3d).
    fn run_detail_report(&self, agent: &str, run_id: &str) -> ToolOutput {
        let detail = match self.log().run_detail(agent, run_id) {
            Some(d) => d,
            None => {
                return ToolOutput::success(format!(
                    "No run with run_id '{run_id}' for agent '{agent}'. Run ids are on every run \
                     line (1e schema) — call again without run_id and copy the id from a recent \
                     run line."
                ))
            }
        };
        let mut out = format!(
            "Run detail for '{agent}' (run {run_id}):\n  {}\n",
            detail.run.describe()
        );
        out.push_str(&format!(
            "  LLM rounds ({}): {} total tokens\n",
            detail.rounds.len(),
            detail.total_round_tokens()
        ));
        for (i, r) in detail.rounds.iter().enumerate() {
            out.push_str(&format!(
                "    {}. {} {} — {} in / {} out, {} thinking chars, {} tool call(s)\n",
                i + 1,
                r.ts.format("%m-%d %H:%M"),
                r.model,
                r.prompt_tokens,
                r.completion_tokens,
                r.thinking_chars,
                r.tool_calls
            ));
        }
        if detail.rounds.is_empty() {
            out.push_str("    (no rounds linked — pre-1e run or usage log missing)\n");
        }
        if detail.feedback.is_empty() {
            out.push_str("  run-level feedback: none\n");
        } else {
            for f in &detail.feedback {
                out.push_str(&format!("  feedback: {}\n", f.describe()));
            }
        }
        ToolOutput::success(out)
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

impl ReadMetricsTool {
    /// 4c: writes the window's raw metric lines (ALL kinds — the same
    /// `days` window as the report, not the report's aggregates) to the
    /// export dir. JSON = an array of the raw lines (fleet mode injects an
    /// `agent` key into each line); CSV = flattened (`kind`,`ts` columns,
    /// `agent` after `ts` in fleet mode, then the union of all remaining
    /// fields in first-seen order — nested values such as a run's per-tool
    /// histogram stay compact JSON, missing fields become empty cells).
    /// Returns `(line_count, file_path)`.
    fn export_lines(
        &self,
        agent: Option<&str>,
        days: u64,
        format: &str,
    ) -> Result<(usize, std::path::PathBuf), String> {
        let log = self.log();
        let since = Utc::now() - Duration::days(days as i64);
        let mut rows: Vec<(Option<String>, MetricsLine)> = Vec::new();
        if let Some(name) = agent {
            // Agent mode: no agent column (the scope already says who).
            for line in log.read_all(name) {
                if line_ts(&line) >= since {
                    rows.push((None, line));
                }
            }
        } else {
            // Fleet mode: every metrics file (agents + skills + fleet),
            // each line tagged with its file's agent name.
            for name in log.agent_names() {
                for line in log.read_all(&name) {
                    if line_ts(&line) >= since {
                        rows.push((Some(name.clone()), line));
                    }
                }
            }
        }
        let dir = self.export_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating export dir: {e}"))?;
        let stem = match agent {
            Some(a) => format!("metrics-{a}"),
            None => "metrics-fleet".to_string(),
        };
        let ts = Utc::now().format("%Y%m%d-%H%M%S");
        let path = dir.join(format!("{stem}-{days}d-{ts}.{format}"));
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|(a, line)| {
                let mut v = serde_json::to_value(line).expect("MetricsLine serializes");
                if let Some(a) = a {
                    if let Some(obj) = v.as_object_mut() {
                        // Overwrites the line's own `agent` field (Check/Eval)
                        // with the file stem — the same value the field
                        // mirrors, and fixes pre-default empty stems.
                        obj.insert("agent".into(), serde_json::Value::String(a.clone()));
                    }
                }
                v
            })
            .collect();
        let content = if format == "json" {
            serde_json::to_string_pretty(&values).map_err(|e| e.to_string())?
        } else {
            export_csv(&values)
        };
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
        Ok((values.len(), path))
    }

    /// 4c: export directory — the test override, or `~/.wuffagent/exports`.
    fn export_dir(&self) -> std::path::PathBuf {
        self.export_dir
            .clone()
            .unwrap_or_else(|| crate::config::get_wuffagent_home().join("exports"))
    }
}

/// 4c: flattens serialized metric lines into CSV (see `export_lines`).
fn export_csv(values: &[serde_json::Value]) -> String {
    // Union of the remaining field names, in first-seen order.
    let mut rest: Vec<String> = Vec::new();
    for v in values {
        let Some(obj) = v.as_object() else {
            continue;
        };
        for k in obj.keys() {
            if k == "kind" || k == "ts" || k == "agent" {
                continue;
            }
            if !rest.iter().any(|r| r == k) {
                rest.push(k.clone());
            }
        }
    }
    let has_agent = values.iter().any(|v| v.get("agent").is_some());
    let mut header: Vec<String> = vec!["kind".to_string(), "ts".to_string()];
    if has_agent {
        header.push("agent".to_string());
    }
    for r in &rest {
        header.push(r.clone());
    }
    let mut out = header.join(",").to_string();
    out.push('\n');
    for v in values {
        let Some(obj) = v.as_object() else {
            continue;
        };
        let mut row: Vec<String> = vec![
            csv_cell(obj.get("kind").and_then(|v| v.as_str()).unwrap_or("")),
            csv_cell(obj.get("ts").and_then(|v| v.as_str()).unwrap_or("")),
        ];
        if has_agent {
            row.push(csv_cell(obj.get("agent").and_then(|v| v.as_str()).unwrap_or("")));
        }
        for r in &rest {
            match obj.get(r.as_str()) {
                Some(serde_json::Value::Null) | None => row.push(String::new()),
                Some(serde_json::Value::String(s)) => row.push(csv_cell(s)),
                Some(other) => row.push(csv_cell(&other.to_string())),
            }
        }
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

/// 4c: RFC-4180 quoting — quote fields containing a comma, quote, or
/// line break; double inner quotes.
fn csv_cell(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
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
