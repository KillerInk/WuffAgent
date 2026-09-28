//! `list_improvement_status`: the auto-improvement loop's own state, readable
//! by an agent (1c).
//!
//! The loop is otherwise passive: after each task, `AgentEngine` decides
//! whether an LLM self-improvement check fires (cooldown + new-evidence
//! gate) and the result lands in the egui review panel. This tool lets an
//! agent answer "when did the last check run, and why is/ isn't one due?" —
//! useful when the agent is editing WuffAgent itself and wants to feed or
//! debug the loop.

use std::sync::Arc;

use chrono::{Duration, Utc};
use crate::agents::metrics::MetricsLog;
use crate::memory::MemoryManager;
use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

/// Fleet-view window when `days` is omitted.
const DEFAULT_DAYS: u64 = 7;
/// Hard ceiling for `days` (same guard as `read_metrics`).
const MAX_DAYS: u64 = 30;
/// Cap on the recent lines shown in the per-agent view.
const MAX_RECENT_LINES: usize = 5;

/// Tool reporting the improvement-loop state (read-only, no LLM call).
pub struct ListImprovementStatusTool {
    memory: Arc<MemoryManager>,
    /// Explicit metrics root (tests); `None` = `MetricsLog::default()`.
    log: Option<MetricsLog>,
}

impl ListImprovementStatusTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory, log: None }
    }

    /// Use an explicit metrics root instead of the default location (tests).
    pub fn with_log(mut self, log: MetricsLog) -> Self {
        self.log = Some(log);
        self
    }

    /// The metrics log for this call (construction is side-effect free, so
    /// rebuilding per call is cheap — same pattern as `read_metrics`).
    fn log(&self) -> MetricsLog {
        match &self.log {
            Some(l) => MetricsLog::new(l.dir().to_path_buf()),
            None => MetricsLog::default(),
        }
    }
}

impl Tool for ListImprovementStatusTool {
    fn name(&self) -> &str {
        "list_improvement_status"
    }

    fn description(&self) -> &str {
        "Show the auto-improvement loop's state: when the last self-improvement check ran, \
         whether new lesson evidence has arrived since, the cooldown/auto_improve settings, \
         and the lesson count. Use it to understand why (no) improvement suggestions appear. \
         Params: agent (optional, profile name) — per-agent state (tasks since its last check, \
         no-op streak/backoff, its evidence gate, last effect verdict) plus that agent's metrics \
         since its last check and its most recent run lines; fleet mode (no agent) adds a Loop \
         status section with per-agent windowed metrics and recent lines. days (optional, \
         fleet mode only) — window in days, default 7, max 30."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_improvement_status".to_string(),
            description: self.description().to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some(std::collections::HashMap::from([
                    (
                        "agent".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Optional: show the per-agent improvement-loop state \
                                          (cooldown counter, no-op streak, evidence, effect verdict, \
                                          metrics since last check) for this profile"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "days".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "integer".to_string(),
                            description: format!(
                                "Fleet view only: window in days for the Loop status section (default {DEFAULT_DAYS}, max {MAX_DAYS})"
                            ),
                            nullable: true,
                        },
                    ),
                ])),
                required: vec![],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let status = self.memory.improvement_status();
        let last_check = format_ago(status.last_check);
        let agent = params
            .get::<String>("agent")
            .filter(|a| !a.is_empty());
        let days = match params.get::<u64>("days") {
            None => DEFAULT_DAYS,
            Some(d) if (1..=MAX_DAYS).contains(&d) => d,
            Some(d) => {
                return Ok(ToolOutput::error(format!(
                    "days must be between 1 and {MAX_DAYS} (got {d})"
                )));
            }
        };

        // 2a: per-agent detail when a profile is named.
        if let Some(name) = &agent {
            let st = status.agents.get(name);
            let known = st.is_some();
            let state = st
                .cloned()
                .unwrap_or_default();
            let mult =
                crate::agents::improvement::no_op_backoff_multiplier(state.no_op_streak.max(1));
            let min_interval = min_interval_label(status.improvement_min_interval_hours);
            // The metrics log (used for the awaiting-samples check below AND
            // the metrics window after the main line).
            let log = self.log();
            // 1b: when the effect check is below its sample floor, surface
            // "awaiting samples" in the verdict field itself, instead of
            // leaving `last_effect_verdict` to read as None/stale.
            let verdict_label =
                match crate::agents::improvement::effect_check_awaiting_samples(
                    &self.memory,
                    name,
                    &log,
                ) {
                    Some((have, need)) => format!(
                        "awaiting samples ({have}/{need}) — not yet judgeable, no verdict recorded"
                    ),
                    None => state
                        .last_effect_verdict
                        .clone()
                        .unwrap_or_else(|| "none (no applied change recorded yet)".to_string()),
                };
            let mut out = format!(
                "Improvement loop for '{name}': auto_improve={}; last check: {}; \
                 tasks since last check: {} (cooldown base {} task(s), backoff x{} = {}, min interval: {}); \
                 no-op streak: {}; new evidence since last check: {}; last effect verdict: {}; \
                 lessons in store: {}{}",
                if status.auto_improve { "on" } else { "off" },
                format_ago(state.last_check),
                state.runs_since_check,
                status.improvement_cooldown_tasks,
                mult,
                status.improvement_cooldown_tasks.saturating_mul(mult as usize),
                min_interval,
                state.no_op_streak,
                if self
                    .memory
                    .has_new_agent_improvement_evidence(name)
                {
                    "yes"
                } else {
                    "no"
                },
                verdict_label,
                status.lesson_count,
                if known {
                    String::new()
                } else {
                    " (no per-agent check recorded yet — state shown as default)"
                        .to_string()
                }
            );
            let since = state.last_check;
            let label = if since.is_some() {
                "since last check"
            } else {
                "all-time (never checked)"
            };
            let window = log.summary_since(name, since).format_labeled(label);
            out.push_str(&format!(
                "\n  metrics {label}: {}",
                if window.is_empty() { "(no data)" } else { &window }
            ));
            let recent = log
                .lines_since(name, since)
                .into_iter()
                .rev()
                .take(MAX_RECENT_LINES);
            for line in recent {
                out.push_str(&format!("\n    {}", line.describe()));
            }
            return Ok(ToolOutput::success(out));
        }

        // Global view (legacy v1 semantics) + a compact per-agent listing.
        let min_interval = min_interval_label(status.improvement_min_interval_hours);
        let mut out = format!(
            "Improvement loop: auto_improve={}; cooldown=at most 1 check per {} completed task(s), \
             min interval: {}; last check (global/legacy): {}; new evidence since last check: {}; \
             lessons in store: {}",
            if status.auto_improve { "on" } else { "off" },
            status.improvement_cooldown_tasks,
            min_interval,
            last_check,
            if status.has_new_evidence { "yes" } else { "no" },
            status.lesson_count,
        );
        if !status.agents.is_empty() {
            out.push_str("\nPer-agent state (2a):");
            for (name, st) in &status.agents {
                out.push_str(&format!(
                    "\n  {name}: last check {}; {} task(s) since; no-op streak {}; verdict {}",
                    format_ago(st.last_check),
                    st.runs_since_check,
                    st.no_op_streak,
                    st.last_effect_verdict
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                ));
            }
        }
        // Loop status section: per-agent windowed metrics + each agent's
        // most recent line (what actually happened since the window started),
        // so the loop state is joinable to real activity without a second
        // read_metrics call.
        let log = self.log();
        let window_since = Utc::now() - Duration::days(days as i64);
        out.push_str(&format!("\nLoop status ({days} day window):"));
        let names = log.agent_names();
        if names.is_empty() {
            out.push_str("\n  (no agents have metrics files yet)");
        } else {
            out.push_str("\n  per-agent metrics (window) + most recent line:");
            for name in &names {
                let label =
                    log.summary_between(name, Some(window_since), None).format_labeled(name);
                match log.lines_since(name, Some(window_since)).pop() {
                    Some(line) => out.push_str(&format!(
                        "\n    {} [last: {}]",
                        if label.is_empty() {
                            format!("{name}: no activity in window")
                        } else {
                            label
                        },
                        line.describe()
                    )),
                    None => out.push_str(&format!(
                        "\n    {}",
                        if label.is_empty() {
                            format!("{name}: no activity in window")
                        } else {
                            label
                        }
                    )),
                }
            }
        }
        Ok(ToolOutput::success(out))
    }
}

/// 2f: render the wall-clock floor: "off" for 0 (disabled), else "Nh".
/// `pub(super)`: shared with the `read_metrics` loop-status view.
pub(super) fn min_interval_label(hours: u32) -> String {
    if hours == 0 {
        "off".to_string()
    } else {
        format!("{hours}h")
    }
}

/// Render a timestamp as "YYYY-MM-DD HH:MM:SS UTC (~N ago)" or "never".
/// `pub(super)`: shared with the `read_metrics` loop-status view.
pub(super) fn format_ago(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match ts {
        Some(ts) => {
            let secs = chrono::Utc::now().timestamp().saturating_sub(ts.timestamp());
            let ago = if secs < 3_600 {
                format!("~{}min ago", secs / 60)
            } else if secs < 86_400 {
                format!("~{}h ago", secs / 3_600)
            } else {
                format!("~{}d ago", secs / 86_400)
            };
            format!(
                "{} ({})",
                ts.format("%Y-%m-%d %H:%M:%S UTC"),
                ago
            )
        }
        None => "never".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryConfig, MemoryEntry, MemoryManager, MemoryType};
    use crate::tools::types::ToolParams;

    /// Fresh manager on a temp dir (isolated improvement_state.json).
    fn fresh_manager() -> (tempfile::TempDir, Arc<MemoryManager>) {
        let dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = Arc::new(MemoryManager::new(config).unwrap());
        (dir, manager)
    }

    fn run(tool: &ListImprovementStatusTool) -> String {
        let out = tool
            .execute(ToolParams {
                values: std::collections::HashMap::new(),
            })
            .expect("status tool must not error");
        match out {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        }
    }

    /// Run with a `days` param (fleet view window).
    fn run_days(tool: &ListImprovementStatusTool, days: u64) -> String {
        let out = tool
            .execute(ToolParams {
                values: std::collections::HashMap::from([(
                    "days".to_string(),
                    serde_json::json!(days),
                )]),
            })
            .expect("status tool must not error");
        match out {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        }
    }

    /// 2a: run with an `agent` param.
    fn run_with_agent(tool: &ListImprovementStatusTool, agent: &str) -> String {
        let out = tool
            .execute(ToolParams {
                values: std::collections::HashMap::from([(
                    "agent".to_string(),
                    serde_json::json!(agent),
                )]),
            })
            .expect("status tool must not error");
        match out {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[test]
    fn test_status_before_any_check() {
        let (_dir, manager) = fresh_manager();
        let tool = ListImprovementStatusTool::new(manager);
        let out = run(&tool);
        assert!(out.contains("auto_improve=on"), "got: {out}");
        assert!(out.contains("last check (global/legacy): never"), "got: {out}");
        assert!(out.contains("new evidence since last check: no"), "got: {out}");
        // 2f: the default (0h) wall-clock floor renders as "off".
        assert!(out.contains("min interval: off"), "got: {out}");
        assert!(out.contains("lessons in store: 0"), "got: {out}");
    }

    #[test]
    fn test_status_after_check_and_lesson() {
        let (_dir, manager) = fresh_manager();
        let tool = ListImprovementStatusTool::new(manager.clone());

        manager.record_improvement_check();
        let out = run(&tool);
        assert!(!out.contains("last check: never"), "got: {out}");
        assert!(out.contains("new evidence since last check: no"), "got: {out}");

        // A lesson NEWER than the recorded check re-arms the evidence gate.
        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "A fresh lesson that should count as new evidence",
                "test",
                &["agent:coder"],
            ))
            .unwrap();
        let out = run(&tool);
        assert!(out.contains("new evidence since last check: yes"), "got: {out}");
        assert!(out.contains("lessons in store: 1"), "got: {out}");
    }

    /// 2a: the optional `agent` param renders the per-agent state (cooldown
    /// counter, backoff, evidence gate, verdict).
    #[test]
    fn test_status_per_agent_detail() {
        let (_dir, manager) = fresh_manager();
        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "A lesson for the named agent",
                "test",
                &["agent:coder"],
            ))
            .unwrap();
        manager.record_agent_task_completed("coder");
        manager.record_agent_task_completed("coder");
        manager.record_agent_improvement_check("coder", false); // streak 1
        manager.record_agent_task_completed("coder");
        manager.record_agent_task_completed("coder");
        manager.record_agent_improvement_check("coder", false); // streak 2 -> x2
        manager.record_agent_task_completed("coder");
        manager.record_effect_verdict("coder", "neutral");

        let tool = ListImprovementStatusTool::new(manager);
        let out = run_with_agent(&tool, "coder");
        assert!(out.contains("Improvement loop for 'coder'"), "got: {out}");
        assert!(out.contains("tasks since last check: 1"), "got: {out}");
        assert!(out.contains("no-op streak: 2"), "got: {out}");
        // Default cooldown base is 5 → x2 = 10.
        assert!(out.contains("backoff x2 = 10"), "got: {out}");
        // 2f: the default (0h) wall-clock floor renders as "off".
        assert!(out.contains("min interval: off"), "got: {out}");
        assert!(out.contains("new evidence since last check: no"), "got: {out}");
        assert!(out.contains("last effect verdict: neutral"), "got: {out}");

        // A profile with no recorded state shows defaults + a hint.
        let out = run_with_agent(&tool, "ghost");
        assert!(out.contains("no per-agent check recorded yet"), "got: {out}");
    }

    /// 2a: the global view lists per-agent states when any exist.
    #[test]
    fn test_status_global_lists_per_agent() {
        let (_dir, manager) = fresh_manager();
        manager.record_agent_improvement_check("coder", true);
        let tool = ListImprovementStatusTool::new(manager);
        let out = run(&tool);
        assert!(out.contains("Per-agent state (2a):"), "got: {out}");
        assert!(out.contains("coder: last check"), "got: {out}");
    }

    #[test]
    fn test_schema_has_no_required_params() {
        let (_dir, manager) = fresh_manager();
        let tool = ListImprovementStatusTool::new(manager);
        let schema = tool.parameters_schema();
        assert_eq!(schema.name, "list_improvement_status");
        assert!(schema.input_type.as_ref().unwrap().required.is_empty());
    }

    /// 2f: a configured wall-clock floor renders as "Nh" in both views (the
    /// default 0 renders as "off" — covered by the tests above).
    #[test]
    fn test_2f_status_shows_configured_min_interval() {
        let dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            improvement_min_interval_hours: 12,
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = Arc::new(MemoryManager::new(config).unwrap());
        manager.record_agent_improvement_check("coder", true);
        manager.record_agent_task_completed("coder");
        let tool = ListImprovementStatusTool::new(manager);

        assert!(run(&tool).contains("min interval: 12h"), "12h → 12h (global)");
        assert!(
            run_with_agent(&tool, "coder").contains("min interval: 12h"),
            "12h → 12h (per-agent)"
        );
    }

    /// The per-agent view joins the loop state to real activity: metrics for
    /// the window since the agent's last check + its most recent lines.
    #[test]
    fn per_agent_view_shows_metrics_since_last_check() {
        let (_dir, manager) = fresh_manager();
        manager.record_agent_improvement_check("coder", false);
        manager.record_agent_task_completed("coder"); // 1 task since the check

        let dir = tempfile::tempdir().unwrap();
        let log = crate::agents::metrics::MetricsLog::new(dir.path());
        log.log_run("coder", 3, 1, 0, 7_000, crate::agents::metrics::RunOutcome::Verified, 11, 4);
        log.log_run(
            "coder",
            2,
            0,
            0,
            9_000,
            crate::agents::metrics::RunOutcome::GaveUp,
            0,
            0,
        );
        log.log_feedback("coder", false);

        let tool = ListImprovementStatusTool::new(manager).with_log(log);
        let out = run_with_agent(&tool, "coder");
        assert!(out.contains("metrics since last check:"), "got: {out}");
        assert!(
            out.contains("metrics since last check: since last check (2 run(s), 5 tool call(s) with 1 errors"),
            "got: {out}"
        );
        assert!(out.contains("outcome: gave_up"), "got: {out}");
        assert!(out.contains("feedback: down"), "got: {out}");
    }

    /// The fleet view's Loop status section honors `days`, lists each agent
    /// with its most recent line, and still surfaces skill usage.
    #[test]
    fn fleet_loop_status_section_honors_days_window() {
        let (_dir, manager) = fresh_manager();
        manager.record_agent_improvement_check("coder", false);

        let dir = tempfile::tempdir().unwrap();
        let log = crate::agents::metrics::MetricsLog::new(dir.path());
        log.log_run("coder", 5, 0, 1, 10_000, crate::agents::metrics::RunOutcome::Verified, 0, 0);

        let tool = ListImprovementStatusTool::new(manager).with_log(log);
        let out = run_days(&tool, 2);
        assert!(out.contains("Loop status (2 day window):"), "got: {out}");
        assert!(out.contains("per-agent metrics (window) + most recent line:"), "got: {out}");
        assert!(
            out.contains("coder (1 run(s), 5 tool call(s) with 0 errors (0.0%)"),
            "got: {out}"
        );
        // The agent's most recent line is joined in, not just the aggregates.
        assert!(out.contains("[last: "), "got: {out}");
        assert!(out.contains("outcome: verified"), "got: {out}");
    }

    /// `days` is out-of-range (or fleet-only) → the per-agent view ignores
    /// it (the window there is anchored to the agent's last check, not
    /// a wall-clock window).
    #[test]
    fn days_param_ignored_in_agent_mode_and_bounded() {
        let (_dir, manager) = fresh_manager();
        manager.record_agent_improvement_check("coder", false);
        let dir = tempfile::tempdir().unwrap();
        let log = crate::agents::metrics::MetricsLog::new(dir.path());
        log.log_run("coder", 1, 0, 0, 1_000, crate::agents::metrics::RunOutcome::Verified, 0, 0);

        let tool = ListImprovementStatusTool::new(manager).with_log(log);
        // In agent mode a VALID `days` must not change the report (the window
        // there is anchored to the agent's last check, not a wall-clock
        // window): with/without the param the output is identical.
        let plain = run_with_agent(&tool, "coder");
        let with_days = tool
            .execute(ToolParams {
                values: std::collections::HashMap::from([
                    ("agent".to_string(), serde_json::json!("coder")),
                    ("days".to_string(), serde_json::json!(2)),
                ]),
            })
            .expect("agent mode must not error on a valid days");
        let text = match with_days {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        };
        assert_eq!(plain, text, "agent mode must ignore `days`");
        assert!(text.contains("metrics since last check:"), "got: {text}");
        assert!(!text.contains("day window"), "got: {text}");

        // Fleet mode enforces the bounds.
        for bad in [0u64, 31u64] {
            match tool.execute(ToolParams {
                values: std::collections::HashMap::from([(
                    "days".to_string(),
                    serde_json::json!(bad),
                )]),
            }) {
                Ok(ToolOutput::Error(e)) => assert!(e.contains("between 1 and 30"), "got: {e}"),
                Ok(other) => panic!("expected days error, got: {:?}", other),
                Err(e) => panic!("unexpected ToolError: {e}"),
            }
        }
    }
}
