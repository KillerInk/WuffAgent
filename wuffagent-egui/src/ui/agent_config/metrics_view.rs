//! F6: the read-only metrics view — allowed-tools tree rendering + the
//! windowed agent metrics block (summary/percentiles/per-tool table/30-day
//! chart/recent lines) + the 4a cross-store run detail.
//!
//! Everything computes from the 2a cached vec (`cached_metrics_lines`), so
//! the per-frame blocks stop doing full JSONL scans while the file is
//! unchanged.

use eframe::egui;
use wuffagent_core::agents::metrics::{
    bucket_summary_from_lines, metrics_report_from_lines, FeedbackKind, MetricsLine,
};
use wuffagent_core::stats::bucket::Granularity;

use super::AgentConfigDialog;

/// 3b: the agent editor's metrics window toggle (the summary + per-tool
/// table recompute from the 2a cached vec; the outcome mini-chart always
/// shows the last 30 days regardless).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MetricsWindow {
    Day7,
    Day30,
    AllTime,
}

impl MetricsWindow {
    /// `None` = unbounded (all-time).
    fn since(&self, now: chrono::DateTime<chrono::Utc>) -> Option<chrono::DateTime<chrono::Utc>> {
        match self {
            MetricsWindow::Day7 => Some(now - chrono::Duration::days(7)),
            MetricsWindow::Day30 => Some(now - chrono::Duration::days(30)),
            MetricsWindow::AllTime => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            MetricsWindow::Day7 => "7d",
            MetricsWindow::Day30 => "30d",
            MetricsWindow::AllTime => "all-time",
        }
    }
}

impl AgentConfigDialog {
    /// 2a: the full metric lines for `agent`, cached by (file len, mtime) —
    /// the per-frame blocks below (recent metrics, evals) stop doing full
    /// JSONL scans every frame while the file is unchanged.
    pub(super) fn cached_metrics_lines(&mut self, agent: &str) -> Vec<MetricsLine> {
        use std::time::SystemTime;
        let log = wuffagent_core::agents::metrics::MetricsLog::default();
        let meta = std::fs::metadata(log.agent_path(agent)).ok();
        let key = match meta.as_ref() {
            Some(m) => (m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
            None => (0, SystemTime::UNIX_EPOCH),
        };
        let hit = self
            .metrics_cache
            .get(agent)
            .filter(|c| c.0 == key.0 && c.1 == key.1);
        if let Some(c) = hit {
            return c.2.clone();
        }
        let lines = log.read_all(agent);
        self.metrics_cache
            .insert(agent.to_string(), (key.0, key.1, lines.clone()));
        lines
    }

    /// Allowed-tools checkboxes + the read-only recent-metrics block (M1).
    pub(super) fn draw_tools_and_metrics(&mut self, ui: &mut egui::Ui, history_agent: Option<&str>) {
        ui.label("Allowed Tools:");

        // Tool checkboxes, grouped: Builtin tools (per category) and
        // MCP tools (per server), each a collapsible group with a
        // "select all" checkbox.
        // Clone first: draw_tool_node takes &mut self, so we can't keep a
        // borrow of self.tools_tree alive across the recursive calls.
        let tree = self.tools_tree.clone();
        for node in &tree {
            self.draw_tool_node(ui, node);
        }

        // M1: read-only recent metrics for the selected EXISTING agent
        // (run/outcome/feedback counts + the last few metric lines) from the
        // per-agent log.
        if let Some(name) = history_agent {
            // 2a: cached lines + pure aggregation (one file read when the
            // file changed, none otherwise — was two full scans per frame).
            let lines = self.cached_metrics_lines(name);
            // The gate stays all-time (the block appears once the agent has
            // ANY metrics); the window toggle only scopes the numbers inside.
            let any = wuffagent_core::agents::metrics::MetricsSummary::from_lines(&lines);
            if any.runs + any.feedback_up + any.feedback_down > 0 {
                ui.separator();
                self.draw_agent_metrics_block(ui, name, &lines);
            }
        }
    }

    /// 3b: the read-only agent metrics block, windowed: 7d/30d/all-time
    /// toggle, the windowed summary + cost + percentiles + model mix, the
    /// per-tool table, a 30-day outcome mini-chart (runs bars + gave-up
    /// line), and the last five metric lines. Everything computes from the
    /// 2a cached vec (no per-frame JSONL scan).
    fn draw_agent_metrics_block(
        &mut self,
        ui: &mut egui::Ui,
        agent: &str,
        lines: &[MetricsLine],
    ) {
        use crate::ui::charts::{fmt_tokens, fmt_usd, index_series, trend_plot};
        use egui_plot::Line;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Metrics:").strong());
            for w in [
                MetricsWindow::Day7,
                MetricsWindow::Day30,
                MetricsWindow::AllTime,
            ] {
                ui.selectable_value(&mut self.metrics_window, w, w.label());
            }
        });

        let now = chrono::Utc::now();
        let report =
            metrics_report_from_lines(lines, self.metrics_window.since(now), None);
        let s = &report.summary;

        let mut summary = format!(
            "{} run(s): {} verified, {} after retry, {} gave up, {} before verification; {} tool call(s) ({} errors); feedback {} up / {} down",
            s.runs,
            s.verified,
            s.verified_after_retry,
            s.gave_up,
            s.not_verified,
            s.tool_calls,
            s.tool_errors,
            s.feedback_up,
            s.feedback_down
        );
        if report.cost_usd > f64::EPSILON {
            summary.push_str(&format!("; cost {}", fmt_usd(report.cost_usd)));
        }
        ui.label(egui::RichText::new(summary).small());

        // Percentiles + model mix (windowed; only when the window has runs).
        if report.p50_duration_ms.is_some() {
            ui.label(
                egui::RichText::new(format!(
                    "p50/p95 duration: {} / {} ms · tokens/run: {} / {}",
                    report.p50_duration_ms.map_or("—".to_string(), |v| v.to_string()),
                    report.p95_duration_ms.map_or("—".to_string(), |v| v.to_string()),
                    report.p50_tokens.map_or("—".to_string(), |v| fmt_tokens(v)),
                    report.p95_tokens.map_or("—".to_string(), |v| fmt_tokens(v)),
                ))
                .weak()
                .small(),
            );
        }
        if !report.model_mix.is_empty() {
            let mix: Vec<String> = report
                .model_mix
                .iter()
                .take(3)
                .map(|(m, n)| format!("{m} ({n})"))
                .collect();
            ui.label(
                egui::RichText::new(format!("models: {}", mix.join(", ")))
                    .weak()
                    .small(),
            );
        }

        // Per-tool table (windowed; only when the window has tool stats).
        if !report.tool_stats.is_empty() {
            let grid = egui::Grid::new(format!("agent_tools_{agent}"))
                .striped(true)
                .spacing(egui::vec2(14.0, 2.0));
            grid.show(ui, |ui| {
                ui.weak("tool");
                ui.weak("calls");
                ui.weak("err %");
                ui.weak("avg ms");
                ui.end_row();
                for t in &report.tool_stats {
                    let err = if t.calls > 0 {
                        100.0 * t.errors as f64 / t.calls as f64
                    } else {
                        0.0
                    };
                    ui.label(t.name.clone());
                    ui.label(t.calls.to_string());
                    ui.label(format!("{err:.0}%"));
                    ui.label(t.avg_ms.to_string());
                    ui.end_row();
                }
            });
        }

        // 30-day outcome mini-chart: runs bars + gave-up line (always the
        // last 30 days; the toggle only scopes the numbers above).
        let buckets = bucket_summary_from_lines(lines, Granularity::Day, now);
        if buckets.iter().any(|b| b.runs > 0) {
            let runs: Vec<f64> = buckets.iter().map(|b| b.runs as f64).collect();
            let gave_up_pts = index_series(
                &buckets.iter().map(|b| b.gave_up as f64).collect::<Vec<_>>(),
            );
            let tick_labels: Vec<String> = buckets
                .iter()
                .map(|b| b.start.format("%m-%d").to_string())
                .collect();
            let mut hovered_x: Option<f64> = None;
            let resp = trend_plot(
                ui,
                &format!("agent_trend_{agent}"),
                buckets.len(),
                120.0,
                Some((index_series(&runs), ui.visuals().hyperlink_color)),
                vec![Line::new("gave up", gave_up_pts).width(1.5)],
                move |i| tick_labels[i].clone(),
                |v| (v as u32).to_string(),
                &mut hovered_x,
            );
            if resp.hovered() {
                if let Some(x) = hovered_x {
                    let i = x.round().clamp(0.0, (buckets.len() - 1) as f64) as usize;
                    let b = &buckets[i];
                    ui.label(
                        egui::RichText::new(format!(
                            "{} — {} run(s) · {} tool call(s) · {} err · {} gave up · {} after retry",
                            b.start.format("%Y-%m-%d"),
                            b.runs,
                            b.tool_calls,
                            b.tool_errors,
                            b.gave_up,
                            b.verified_after_retry
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
        }

        // The last five lines, newest first (window-independent) — 3d: 👍/👎
        // on each recent RUN row writes a run-level feedback line linked via
        // the 1e run_id; the pressed rating stays highlighted (matched from
        // the Feedback lines of the same file).
        ui.group(|ui| {
            ui.label(
                egui::RichText::new("Recent metric lines (latest 5):").strong(),
            );
            for l in lines.iter().rev().take(5) {
                let run_id: &str = match l {
                    MetricsLine::Run { run_id: rid, .. } if !rid.is_empty() => rid,
                    _ => "",
                };
                let expanded = self.expanded_run_id.as_deref() == Some(run_id);
                ui.horizontal(|ui| {
                    if !run_id.is_empty() {
                        let arrow = if expanded { "▾ " } else { "▸ " };
                        ui.button(egui::RichText::new(arrow).weak().small())
                            .on_hover_text("Show this run's LLM rounds + feedback (cross-store view)")
                            .clicked()
                            .then(|| {
                                self.expanded_run_id =
                                    if expanded { None } else { Some(run_id.to_string()) };
                                self.run_detail = None;
                            });
                    }
                    ui.label(egui::RichText::new(l.describe()).weak().small());
                    if run_id.is_empty() {
                        return;
                    }
                    let rated = lines.iter().find_map(|f| match f {
                        MetricsLine::Feedback {
                            feedback,
                            run_id: Some(frid),
                            ..
                        } if frid == run_id => Some(*feedback),
                        _ => None,
                    });
                    let up = ui
                        .selectable_label(rated == Some(FeedbackKind::Up), "👍")
                        .on_hover_text("Rate this run: good");
                    let down = ui
                        .selectable_label(rated == Some(FeedbackKind::Down), "👎")
                        .on_hover_text("Rate this run: bad");
                    if up.clicked() || down.clicked() {
                        wuffagent_core::agents::metrics::MetricsLog::default()
                            .log_feedback_run(agent, run_id, up.clicked());
                    }
                });
                // 4a: the expanded cross-store view under the run row.
                if expanded {
                    self.draw_run_detail(ui, agent, run_id);
                }
            }
        });
    }

    /// 4a: the expandable cross-store detail under a recent run row — the
    /// run's LLM rounds (usage.jsonl entries carrying the same 1e run_id,
    /// in log order) plus its run-level feedback. Loaded lazily and
    /// re-fetched only when the usage file grew (new rounds) or a different
    /// run was expanded.
    fn draw_run_detail(&mut self, ui: &mut egui::Ui, agent: &str, run_id: &str) {
        let usage_path =
            wuffagent_core::usage::recorder::UsageRecorder::usage_log_path();
        let usage_len = std::fs::metadata(&usage_path).map(|m| m.len()).unwrap_or(0);
        let stale = !self
            .run_detail
            .as_ref()
            .is_some_and(|(id, len, _)| id == run_id && *len == usage_len);
        if stale {
            let detail =
                wuffagent_core::agents::metrics::MetricsLog::default().run_detail(agent, run_id);
            self.run_detail = detail.map(|d| (run_id.to_string(), usage_len, d));
        }
        let Some((_, _, detail)) = &self.run_detail else {
            return;
        };
        ui.label(
            egui::RichText::new(format!(
                "    LLM rounds ({}): {} total tokens",
                detail.rounds.len(),
                detail.total_round_tokens()
            ))
            .weak()
            .small(),
        );
        for (i, r) in detail.rounds.iter().enumerate() {
            ui.label(
                egui::RichText::new(format!(
                    "      {}. {} {} — {} in / {} out, {} thinking chars, {} tool call(s)",
                    i + 1,
                    r.ts.format("%m-%d %H:%M"),
                    r.model,
                    r.prompt_tokens,
                    r.completion_tokens,
                    r.thinking_chars,
                    r.tool_calls
                ))
                .weak()
                .small(),
            );
        }
        if detail.rounds.is_empty() {
            ui.label(
                egui::RichText::new("      (no rounds linked — pre-1e run or usage log missing)")
                    .weak()
                    .small(),
            );
        }
        if detail.feedback.is_empty() {
            ui.label(egui::RichText::new("    run-level feedback: none").weak().small());
        } else {
            for f in &detail.feedback {
                ui.label(
                    egui::RichText::new(format!("    feedback: {}", f.describe()))
                        .weak()
                        .small(),
                );
            }
        }
    }
}
