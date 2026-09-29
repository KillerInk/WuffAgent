//! 3a: fleet loop status — the core shared by the `read_metrics
//! status=true` tool and the egui fleet dashboard, so the two surfaces
//! cannot diverge.

use chrono::{DateTime, Utc};

use super::aggregates::MetricsSummary;
use super::log::MetricsLog;

/// 3a: the loop half of the fleet status — cost-control settings and the
/// global (legacy) baseline. `None` in [`FleetLoopStatus`] when no memory
/// manager was wired (standalone tools/tests degrade to metrics only).
#[derive(Debug, Clone, Default)]
pub struct LoopConfigInfo {
    pub auto_improve: bool,
    pub cooldown_tasks: usize,
    pub min_interval_hours: u32,
    pub lessons: usize,
    /// Global (legacy) last check; per-agent baselines live on the rows.
    pub last_check: Option<DateTime<Utc>>,
    /// Whether new Lesson evidence exists since the global last check.
    pub new_evidence: bool,
}

/// 3a: one agent row of the fleet status — its loop state (if ever
/// checked) joined with its windowed metrics and its most recent line
/// (`describe()` text — what actually happened in the window).
#[derive(Debug, Clone)]
pub struct FleetAgentStatus {
    pub name: String,
    pub loop_state: Option<crate::memory::types::AgentImprovementState>,
    /// Windowed metrics (`ts >= since`); all-zero when the agent had no
    /// lines in the window.
    pub summary: MetricsSummary,
    /// The window's newest line rendered for humans (None = none).
    pub last_line: Option<String>,
}

/// 3a: fleet-wide token spend over the window (from the metrics store —
/// the runs the loop improves, not the improvement checks' own calls).
#[derive(Debug, Clone, Copy, Default)]
pub struct FleetSpend {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub runs: u32,
}

/// 3a: structured fleet loop status — the single implementation behind
/// BOTH the `read_metrics status=true` tool (renders it as text) and the
/// egui fleet dashboard (draws it), so the two surfaces cannot diverge.
#[derive(Debug, Clone, Default)]
pub struct FleetLoopStatus {
    pub window_days: u64,
    pub loop_config: Option<LoopConfigInfo>,
    /// One row per agent in the UNION of the metrics store's agent files
    /// and the memory manager's per-agent loop states (name-sorted) —
    /// loop-only agents still appear (with all-zero window metrics).
    pub agents: Vec<FleetAgentStatus>,
    pub fleet_spend: FleetSpend,
}

/// 3a: build the structured fleet loop status over the last `days` days.
/// Read-only (file reads + in-memory state); the window is `[now-days, now)`.
pub fn fleet_loop_status(
    log: &MetricsLog,
    memory: Option<&crate::memory::MemoryManager>,
    days: u64,
) -> FleetLoopStatus {
    let st = memory.map(|m| m.improvement_status());
    let loop_config = st.as_ref().map(|st| LoopConfigInfo {
        auto_improve: st.auto_improve,
        cooldown_tasks: st.improvement_cooldown_tasks,
        min_interval_hours: st.improvement_min_interval_hours,
        lessons: st.lesson_count,
        last_check: st.last_check,
        new_evidence: st.has_new_evidence,
    });

    let mut names: std::collections::BTreeSet<String> =
        log.agent_names().into_iter().collect();
    if let Some(st) = &st {
        for name in st.agents.keys() {
            names.insert(name.clone());
        }
    }

    let since = Utc::now() - chrono::Duration::days(days as i64);
    let mut agents = Vec::new();
    let mut spend = FleetSpend::default();
    for name in names {
        let summary = log.summary_between(&name, Some(since), None);
        spend.tokens_in += summary.tokens_in;
        spend.tokens_out += summary.tokens_out;
        spend.runs += summary.runs;
        let last_line = log.lines_since(&name, Some(since)).pop().map(|l| l.describe());
        let loop_state = st.as_ref().and_then(|s| s.agents.get(&name).cloned());
        agents.push(FleetAgentStatus {
            name,
            loop_state,
            summary,
            last_line,
        });
    }
    FleetLoopStatus {
        window_days: days,
        loop_config,
        agents,
        fleet_spend: spend,
    }
}
