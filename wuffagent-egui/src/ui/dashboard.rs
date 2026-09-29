//! 3a: fleet dashboard panel — a floating window with (1) a per-agent KPI
//! card row (runs / outcome % / tool-error % / tokens + $ / last
//! activity), (2) a 30-day fleet trend chart (runs bars + tool-error-rate
//! line), and (3) a compact improvement-loop state table (last check,
//! tasks since, no-op streak, verdict, window metrics).
//!
//! Data is the SHARED core `fleet_loop_status` — the same implementation
//! the `read_metrics status=true` tool renders as text — plus per-agent
//! `MetricsReport`s (window cost/tokens). Loaded on first open, refreshed
//! when `mark_dirty` fires (stream completions) or the window is toggled.
use eframe::egui;
use egui_plot::Line;

use super::charts::{ago, fmt_tokens, fmt_usd, index_series, trend_plot};
use super::theme::Theme;
use wuffagent_core::agents::metrics::{fleet_loop_status, MetricsLog};
use wuffagent_core::memory::MemoryManager;
use wuffagent_core::stats::bucket::Granularity;

/// One day of the fleet trend (daily buckets summed across agents).
#[derive(Clone, Copy, Debug, Default)]
struct TrendDay {
    start: chrono::NaiveDateTime,
    runs: u32,
    tool_calls: u32,
    tool_errors: u32,
    gave_up: u32,
}

impl TrendDay {
    /// Tool-call error rate in percent (0 when no calls).
    fn err_pct(&self) -> f64 {
        if self.tool_calls > 0 {
            100.0 * self.tool_errors as f64 / self.tool_calls as f64
        } else {
            0.0
        }
    }
}

/// 3a: per-agent KPI card data (windowed report + last activity).
#[derive(Clone, Debug)]
struct Kpi {
    name: String,
    report: wuffagent_core::agents::metrics::MetricsReport,
    last_activity: Option<chrono::DateTime<chrono::Utc>>,
}

/// Floating fleet dashboard window (mirrors [`super::usage_panel::UsagePanel`]'s
/// show/dirty pattern).
pub struct FleetDashboard {
    pub show: bool,
    /// Aggregation window in days (7 | 30; default 30).
    window: u64,
    /// Window the cached data below was built for (a toggle rebuilds it).
    data_window: u64,
    /// Set on stream completions: the next frame re-reads the stores.
    dirty: bool,
    /// Cached `fleet_loop_status` for `data_window`.
    status: Option<wuffagent_core::agents::metrics::FleetLoopStatus>,
    /// Per-agent windowed reports (KPI cards).
    kpis: Vec<Kpi>,
    /// Daily fleet trend (runs + error rate).
    trend: Vec<TrendDay>,
}

impl Default for FleetDashboard {
    fn default() -> Self {
        Self::new()
    }
}

impl FleetDashboard {
    pub fn new() -> Self {
        Self {
            show: false,
            window: 30,
            data_window: 30,
            dirty: true,
            status: None,
            kpis: Vec::new(),
            trend: Vec::new(),
        }
    }

    /// Flag the panel to re-read the stores (called on stream completions).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Rebuild the cached data for the current window. Read-only (file
    /// reads + in-memory state); cheap enough to run per dirty frame.
    fn refresh(&mut self, memory: &MemoryManager) {
        let log = MetricsLog::default();
        let status = fleet_loop_status(&log, Some(memory), self.window);
        let now = chrono::Utc::now();
        let since = now - chrono::Duration::days(self.window as i64);
        let names: Vec<String> = log.agent_names();

        let mut kpis = Vec::new();
        for name in &names {
            kpis.push(Kpi {
                name: name.clone(),
                report: log.report(name, Some(since), None),
                last_activity: log.last_activity(name),
            });
        }

        // Sum the per-agent daily buckets into fleet-wide days.
        let mut trend: Vec<TrendDay> = Vec::new();
        for name in &names {
            let buckets = log.bucket_summary(name, Granularity::Day, now);
            if trend.is_empty() {
                trend = buckets
                    .iter()
                    .map(|b| TrendDay {
                        start: b.start,
                        ..Default::default()
                    })
                    .collect();
            }
            for (t, b) in trend.iter_mut().zip(&buckets) {
                t.runs += b.runs;
                t.tool_calls += b.tool_calls;
                t.tool_errors += b.tool_errors;
                t.gave_up += b.gave_up;
            }
        }

        self.status = Some(status);
        self.kpis = kpis;
        self.trend = trend;
        self.data_window = self.window;
        self.dirty = false;
    }

    /// Draw the fleet dashboard window. No-op when not shown.
    pub fn draw(&mut self, ctx: &egui::Context, theme: &Theme, memory: &MemoryManager) {
        if !self.show {
            return;
        }

        // Lazy load / refresh on dirty or window toggle.
        if self.dirty || self.status.is_none() || self.data_window != self.window {
            self.refresh(memory);
        }

        let mut open = self.show;
        let window = self.window;

        egui::Window::new("Fleet Dashboard")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_size([780.0, 600.0])
            .min_size([520.0, 420.0])
            .show(ctx, |ui| {
                ui.visuals_mut().panel_fill = theme.surface;

                // The whole body scrolls: in a small window the KPI cards,
                // the trend chart and the loop table must not be clipped.
                egui::ScrollArea::vertical().show(ui, |ui| {
                // Header: title left, window label right.
                ui.horizontal(|ui| {
                    ui.heading("Fleet Dashboard");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("last {window} days")).weak());
                    });
                });

                // Window toggle.
                ui.horizontal(|ui| {
                    for days in [7u64, 30] {
                        let active = self.window == days;
                        let fill = if active {
                            theme.selected_bg
                        } else {
                            theme.surface_light
                        };
                        if ui
                            .add(
                                egui::Button::new(format!("{days}d"))
                                    .fill(fill)
                                    .corner_radius(4)
                                    .min_size(egui::vec2(48.0, 0.0)),
                            )
                            .on_hover_text("Aggregation window")
                            .clicked()
                        {
                            self.window = days;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("window:").weak());
                    });
                });

                // Fleet-wide window totals (runs, tokens, spend).
                if let Some(status) = &self.status {
                    let spend = status.fleet_spend;
                    let cost: f64 = self.kpis.iter().map(|k| k.report.cost_usd).sum();
                    ui.horizontal(|ui| {
                        ui.add_space(2.0);
                        kpi_stat(ui, "runs", spend.runs.to_string(), theme.primary, theme);
                        kpi_stat(
                            ui,
                            "tokens",
                            fmt_tokens(spend.tokens_in + spend.tokens_out),
                            theme.accent,
                            theme,
                        );
                        kpi_stat(ui, "spend", fmt_usd(cost), theme.warning, theme);
                        ui.add_space(2.0);
                    });
                }

                // ── Per-agent KPI cards ──
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("agents (window)").color(theme.text_secondary).strong(),
                );
                ui.add_space(2.0);
                if self.kpis.is_empty() {
                    ui.label(
                        egui::RichText::new("no agent activity recorded yet").color(theme.text_secondary),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for k in &self.kpis {
                            agent_kpi_card(ui, k, theme);
                        }
                    });
                }

                // ── 30-day fleet trend ──
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("fleet trend (daily)").color(theme.text_secondary).strong(),
                );
                ui.add_space(2.0);
                self.draw_trend(ui, theme);

                // ── Improvement-loop state table ──
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("improvement loop").color(theme.text_secondary).strong(),
                );
                ui.add_space(2.0);
                self.draw_loop_table(ui, theme);

                // Footnote.
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "data: ~/.wuffagent/metrics (shared core with the read_metrics status tool)",
                    )
                    .weak(),
                );
                });
            });

        // The ✕ in the title bar may have cleared the flag.
        self.show = open;
    }

    /// The daily fleet trend: runs bars + tool-error-rate line (scaled to
    /// the runs axis so both series share one plot).
    fn draw_trend(&self, ui: &mut egui::Ui, theme: &Theme) {
        let n = self.trend.len();
        if n == 0 || self.trend.iter().all(|d| d.runs == 0) {
            ui.label(
                egui::RichText::new("no runs recorded in this window yet").color(theme.text_secondary),
            );
            return;
        }

        let runs: Vec<f64> = self.trend.iter().map(|d| d.runs as f64).collect();
        let max_runs = runs.iter().cloned().fold(0.0, f64::max).max(1.0);
        let err: Vec<f64> = self
            .trend
            .iter()
            .map(|d| d.err_pct().min(100.0) * max_runs / 100.0)
            .collect();

        let bars = index_series(&runs);
        let err_line = index_series(&err);

        // Hover tracking: the plot captures the pointer's data-x into
        // `hovered_x` (trend_plot's `pointer_out`); the details line below
        // shows the bucket under the pointer.
        let mut hovered_x: Option<f64> = None;
        // Axis-tick labels are 'static (egui_plot's formatter bound), so
        // precompute them as owned strings instead of borrowing `self`.
        let tick_labels: Vec<String> = self
            .trend
            .iter()
            .map(|d| d.start.format("%m-%d").to_string())
            .collect();
        let plot_resp = trend_plot(
            ui,
            "fleet_trend_plot",
            n,
            // Clamped: inside the dashboard's ScrollArea the available
            // height is infinite, and a giant chart would push the loop
            // table far below the visible area.
            (ui.available_height() - 220.0).clamp(160.0, 360.0),
            Some((bars, theme.primary)),
            vec![Line::new("tool errors %", err_line)
                .color(theme.warning)
                .width(1.5)],
            move |i| tick_labels[i].clone(),
            |v| (v as u32).to_string(),
            &mut hovered_x,
        );

        if plot_resp.hovered() {
            if let Some(x) = hovered_x {
                let i = x.round().clamp(0.0, (n - 1) as f64) as usize;
                let d = &self.trend[i];
                ui.label(
                    egui::RichText::new(format!(
                        "{} — {} run(s) · {} tool call(s) · {} err ({}%) · {} gave up",
                        d.start.format("%Y-%m-%d"),
                        d.runs,
                        d.tool_calls,
                        d.tool_errors,
                        d.err_pct().round(),
                        d.gave_up,
                    ))
                    .color(theme.text_primary),
                );
            }
        } else {
            ui.label(egui::RichText::new("hover the chart for daily details").weak());
        }
    }

    /// The improvement-loop state table: one row per agent from the shared
    /// `fleet_loop_status` (loop state + window metrics + last line).
    fn draw_loop_table(&self, ui: &mut egui::Ui, theme: &Theme) {
        let Some(status) = &self.status else {
            return;
        };

        // Global loop settings (cost control).
        match &status.loop_config {
            Some(cfg) => {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "auto_improve {} · cooldown {} task(s) · min interval {} · lessons {} · last check {}",
                            if cfg.auto_improve { "on" } else { "off" },
                            cfg.cooldown_tasks,
                            if cfg.min_interval_hours >= 24 {
                                format!("{}d", cfg.min_interval_hours / 24)
                            } else {
                                format!("{}h", cfg.min_interval_hours)
                            },
                            cfg.lessons,
                            ago(cfg.last_check),
                        ))
                        .color(theme.text_secondary)
                        .weak(),
                    );
                });
                ui.add_space(4.0);
            }
            None => {
                ui.label(
                    egui::RichText::new("loop: no memory manager wired (metrics only)")
                        .weak()
                        .color(theme.text_secondary),
                );
                ui.add_space(4.0);
            }
        }

        if status.agents.is_empty() {
            ui.label(
                egui::RichText::new("no agents recorded yet").color(theme.text_secondary),
            );
            return;
        }

        let grid = egui::Grid::new("fleet_loop_table")
            .striped(true)
            .spacing(egui::vec2(14.0, 3.0));
        // Horizontal scroll: the verdict column can make the 7-column grid
        // wider than a narrow window (min 520px) — scroll instead of clip.
        egui::ScrollArea::horizontal().show(ui, |ui| {
            grid.show(ui, |ui| {
            ui.weak("agent");
            ui.weak("last check");
            ui.weak("tasks since");
            ui.weak("streak");
            ui.weak("verdict");
            ui.weak("runs (win)");
            ui.weak("err % (win)");
            ui.end_row();

            for a in &status.agents {
                let err = if a.summary.tool_calls > 0 {
                    format!("{:.1}%", 100.0 * a.summary.tool_errors as f64 / a.summary.tool_calls as f64)
                } else {
                    "—".to_string()
                };
                ui.label(a.name.clone());
                ui.label(ago(a.loop_state.as_ref().and_then(|ls| ls.last_check)));
                ui.label(
                    a.loop_state
                        .as_ref()
                        .map(|ls| ls.runs_since_check.to_string())
                        .unwrap_or_else(|| "—".to_string()),
                );
                ui.label(
                    a.loop_state
                        .as_ref()
                        .map(|ls| ls.no_op_streak.to_string())
                        .unwrap_or_else(|| "—".to_string()),
                );
                let verdict = a
                    .loop_state
                    .as_ref()
                    .and_then(|ls| ls.last_effect_verdict.clone())
                    .unwrap_or_else(|| "—".to_string());
                let verdict_dim = verdict == "—";
                ui.label(
                    egui::RichText::new(verdict).color(if verdict_dim {
                        theme.text_secondary
                    } else {
                        theme.text_primary
                    }),
                );
                ui.label(a.summary.runs.to_string());
                ui.label(err);
                ui.end_row();
            }
            });
        });
    }
}

/// One per-agent KPI card: name + window runs/outcome/error + tokens + $
/// + last activity.
fn agent_kpi_card(ui: &mut egui::Ui, k: &Kpi, theme: &Theme) {
    egui::Frame::new()
        .fill(theme.surface_light)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .corner_radius(7.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            // The frame inherits the parent (horizontal_wrapped) layout —
            // force a vertical stack so the card is a compact block that
            // wraps instead of overflowing the row.
            ui.vertical(|ui| {
                ui.set_min_width(150.0);
                ui.label(egui::RichText::new(k.name.clone()).strong().size(13.0));
                ui.add_space(2.0);

                let r = &k.report;
                let verified_pct = if r.summary.runs > 0 {
                    100.0
                        * (r.summary.verified + r.summary.verified_after_retry) as f64
                        / r.summary.runs as f64
                } else {
                    0.0
                };
                let err_pct = if r.summary.tool_calls > 0 {
                    100.0 * r.summary.tool_errors as f64 / r.summary.tool_calls as f64
                } else {
                    0.0
                };

                kpi_row(ui, "runs", r.summary.runs.to_string(), theme.text_primary);
                kpi_row(
                    ui,
                    "outcome",
                    if r.summary.runs > 0 {
                        format!("{verified_pct:.0}% verified")
                    } else {
                        "—".to_string()
                    },
                    theme.success,
                );
                kpi_row(
                    ui,
                    "tools err",
                    if r.summary.tool_calls > 0 {
                        format!("{err_pct:.1}%")
                    } else {
                        "—".to_string()
                    },
                    if err_pct > 25.0 && r.summary.tool_calls >= 10 {
                        theme.error
                    } else {
                        theme.text_primary
                    },
                );
                kpi_row(
                    ui,
                    "tokens",
                    format!(
                        "{} in / {} out",
                        fmt_tokens(r.summary.tokens_in),
                        fmt_tokens(r.summary.tokens_out)
                    ),
                    theme.accent,
                );
                kpi_row(ui, "spend", fmt_usd(r.cost_usd), theme.warning);
                kpi_row(
                    ui,
                    "last active",
                    ago(k.last_activity),
                    theme.text_secondary,
                );
            });
        });
    ui.add_space(8.0);
}

/// A small fleet-wide stat card (label + value, stacked vertically).
fn kpi_stat(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    value_color: egui::Color32,
    theme: &Theme,
) {
    egui::Frame::new()
        .fill(theme.surface_light)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .corner_radius(7.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            // The frame inherits the parent (horizontal) layout — force the
            // label/value to stack vertically.
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(label).weak().size(10.5));
                ui.add_space(1.0);
                ui.label(
                    egui::RichText::new(value).strong().size(16.0).color(value_color),
                );
            });
        });
    ui.add_space(7.0);
}

/// A label/value line inside an agent KPI card.
fn kpi_row(ui: &mut egui::Ui, label: &str, value: String, value_color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.set_min_width(64.0);
        ui.label(egui::RichText::new(label).weak().size(11.0));
        ui.label(
            egui::RichText::new(value).size(11.5).color(value_color),
        );
    });
    ui.add_space(1.0);
}
