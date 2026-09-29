//! `read_metrics` report rendering (phase 4): the agent-mode, fleet-mode,
//! loop-status and run-detail report bodies of `ReadMetricsTool` (the tool
//! struct + `Tool` impl live in `super`).

use chrono::{Duration, Utc};
use crate::agents::metrics::{MetricsLine, fleet_loop_status};
use crate::tools::types::ToolOutput;

use super::{ReadMetricsTool, MAX_RECENT_LINES, OUTSIDE_WINDOW_LINES};

impl ReadMetricsTool {
    /// Agent-mode report: aggregates over the window + newest raw lines.
    pub(crate) fn agent_report(&self, agent: &str, days: u64, compare: bool) -> ToolOutput {
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
        let window: Vec<&MetricsLine> = all.iter().filter(|l| l.ts() >= since).collect();

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
    pub(crate) fn fleet_report(&self, days: u64) -> ToolOutput {
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
    pub(crate) fn fleet_status_report(&self, days: u64) -> ToolOutput {
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
                    crate::tools::builtin::improvement::status::min_interval_label(cfg.min_interval_hours),
                    cfg.lessons,
                ));
                out.push_str(&format!(
                    "\n  last check (global/legacy): {}",
                    crate::tools::builtin::improvement::status::format_ago(cfg.last_check)
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
                                crate::tools::builtin::improvement::status::format_ago(ls.last_check),
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

impl ReadMetricsTool {
    /// 4a: the cross-store join view of one run — its own metrics line, its
    /// LLM rounds (usage.jsonl entries carrying the same 1e run_id), and its
    /// run-level feedback (3d).
    pub(crate) fn run_detail_report(&self, agent: &str, run_id: &str) -> ToolOutput {
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
