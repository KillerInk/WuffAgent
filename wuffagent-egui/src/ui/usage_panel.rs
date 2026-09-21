use eframe::egui;
use egui_plot::{GridMark, Line, Legend, Plot};

use super::theme::Theme;
use wuffagent_core::usage::recorder::UsageRecorder;
use wuffagent_core::usage::stats::{bucketize, Bucket, Granularity, UsageLogReader};

/// Aggregation window shown in the chart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    const OPTIONS: [(&'static str, Self); 3] =
        [("Hour", Self::Hour), ("Day", Self::Day), ("Week", Self::Week)];

    fn granularity(self) -> Granularity {
        match self {
            Range::Hour => Granularity::Hour,
            Range::Day => Granularity::Day,
            Range::Week => Granularity::Week,
        }
    }

    fn window_label(self) -> &'static str {
        match self {
            Range::Hour => "last 24 hours",
            Range::Day => "last 30 days",
            Range::Week => "last 12 weeks",
        }
    }
}

/// Floating panel with a per-bucket line chart of LLM token usage
/// (input / output / total), aggregated from `~/.wuffagent/usage.jsonl`.
///
/// Data is loaded lazily on first open and then incrementally: stream
/// completions mark the panel dirty (`mark_dirty`), and the next frame
/// appends only new lines from the log file (full rescan if the file
/// shrank — e.g. the user deleted it).
pub struct UsagePanel {
    pub show_panel: bool,
    range: Range,
    /// Incremental reader state (tracks how many bytes were consumed).
    reader: UsageLogReader,
    /// All parsed log entries (in file order).
    entries: Vec<wuffagent_core::usage::recorder::UsageEntry>,
    /// Set by the event handler on stream completions: on the next frame
    /// the panel polls the log file for new lines.
    dirty: bool,
    /// True once the initial full load has happened.
    loaded: bool,
}

impl Default for UsagePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl UsagePanel {
    pub fn new() -> Self {
        Self {
            show_panel: false,
            range: Range::Day,
            reader: UsageLogReader::new(),
            entries: Vec::new(),
            dirty: true,
            loaded: false,
        }
    }

    /// Flag the panel to pick up newly logged calls (called on stream
    /// completions). Cheap: no I/O until the panel draws its next frame.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Draw the usage panel window. No-op when not shown.
    pub fn draw(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.show_panel {
            return;
        }

        // Lazy load / incremental refresh. Best-effort: a missing or
        // unreadable file just yields an (empty) chart state.
        if self.dirty || !self.loaded {
            let path = UsageRecorder::default_path();
            if !self.loaded {
                let (entries, _skipped) = self.reader.load_all(&path);
                self.entries = entries;
                self.loaded = true;
            } else {
                let (new_entries, _skipped) = self.reader.poll(&path);
                if !new_entries.is_empty() {
                    self.entries.extend(new_entries);
                }
            }
            self.dirty = false;
        }

        let range = self.range;

        // egui only draws the title-bar ✕ when `open` is provided; a local
        // bool keeps the borrow checker happy (the content closure below
        // borrows `self` for the range selector and data).
        let mut open = self.show_panel;

        egui::Window::new("Token Usage")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_size([560.0, 420.0])
            .min_size([380.0, 260.0])
            .show(ctx, |ui| {
                ui.visuals_mut().panel_fill = theme.surface;

                ui.horizontal(|ui| {
                    ui.heading("Token Usage");
                    ui.label(egui::RichText::new(range.window_label()).weak());
                });

                // Range selector.
                ui.horizontal(|ui| {
                    for (label, r) in Range::OPTIONS {
                        let active = self.range == r;
                        let fill = if active {
                            theme.selected_bg
                        } else {
                            theme.surface_light
                        };
                        if ui
                            .add(
                                egui::Button::new(label)
                                    .fill(fill)
                                    .corner_radius(4)
                                    .min_size(egui::vec2(48.0, 0.0)),
                            )
                            .on_hover_text("Bucket size for the chart")
                            .clicked()
                        {
                            self.range = r;
                        }
                    }
                });
                ui.separator();

                let window = bucketize(&self.entries, range.granularity(), chrono::Utc::now());

                if window.calls == 0 {
                    // Empty state.
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("No usage data in this window yet.")
                                .color(theme.text_secondary),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Calls are logged after each chat round; open a session and ask something.",
                            )
                            .weak(),
                        );
                    });
                    ui.add_space(8.0);
                } else {
                    // Build the three series: one point per bucket.
                    let n = window.buckets.len();
                    let input_pts = series_pts(&window.buckets, |b| b.prompt_tokens as f64);
                    let output_pts = series_pts(&window.buckets, |b| b.completion_tokens as f64);
                    let total_pts = series_pts(&window.buckets, |b| b.total_tokens as f64);

                    // Hover tracking: capture the pointer's plot x (bucket
                    // index) from inside the plot, then show details below.
                    let mut hovered_x: Option<f64> = None;
                    let buckets = &window.buckets;
                    let gran = range.granularity();

                    let plot_resp = Plot::new("usage_plot")
                        .min_size(egui::vec2(0.0, 170.0))
                        .legend(Legend::default())
                        .allow_zoom(false)
                        .allow_drag(false)
                        .x_axis_formatter(move |mark: GridMark, _bounds: &std::ops::RangeInclusive<f64>| {
                            let i = (mark.value.round()).clamp(0.0, (n - 1) as f64) as usize;
                            buckets[i].start.format("%H:%M").to_string()
                        })
                        .y_axis_formatter(|mark: GridMark, _bounds: &std::ops::RangeInclusive<f64>| {
                            fmt_tokens(mark.value.max(0.0) as u64)
                        })
                        .show(ui, |pui| {
                            pui.line(Line::new("input", input_pts).color(theme.primary).width(1.5));
                            pui.line(
                                Line::new("output", output_pts)
                                    .color(theme.accent)
                                    .width(1.5),
                            );
                            pui.line(
                                Line::new("total", total_pts)
                                    .color(theme.warning)
                                    .width(2.0),
                            );
                            if let Some(p) = pui.pointer_coordinate() {
                                hovered_x = Some(p.x);
                            }
                        });

                    // Hover tooltip: details for the bucket under the pointer.
                    if plot_resp.response.hovered() {
                        if let Some(x) = hovered_x {
                            let i = x.round().clamp(0.0, (n - 1) as f64) as usize;
                            let b = &buckets[i];
                            let stamp = match gran {
                                Granularity::Hour => b.start.format("%Y-%m-%d %H:%M"),
                                Granularity::Day => b.start.format("%Y-%m-%d"),
                                Granularity::Week => b.start.format("%Y-%m-%d (Mon)"),
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} — in {} · out {} · total {} · {} call{}",
                                    stamp,
                                    fmt_tokens(b.prompt_tokens),
                                    fmt_tokens(b.completion_tokens),
                                    fmt_tokens(b.total_tokens),
                                    b.calls,
                                    if b.calls == 1 { "" } else { "s" },
                                ))
                                .color(theme.text_primary),
                            );
                        }
                    } else {
                        ui.label(egui::RichText::new("hover the chart for bucket details").weak());
                    }
                }

                ui.separator();

                // Summary line.
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} in · {} out · {} total · {} call{}",
                            fmt_tokens(window.total_prompt_tokens),
                            fmt_tokens(window.total_completion_tokens),
                            fmt_tokens(window.total_tokens),
                            window.calls,
                            if window.calls == 1 { "" } else { "s" },
                        ))
                        .strong(),
                    );
                    ui.label(egui::RichText::new(range.window_label()).weak());
                });

                // Footnote: what a token roughly is.
                ui.label(
                    egui::RichText::new("1 token ≈ 4 characters of text (server-reported counts)")
                        .weak(),
                );
            });

        // The ✕ in the title bar (egui's `open`) may have cleared the flag.
        self.show_panel = open;
    }
}

/// Build one chart series: x = bucket index, y = `get(bucket)`.
fn series_pts(buckets: &[Bucket], get: impl Fn(&Bucket) -> f64) -> Vec<[f64; 2]> {
    buckets
        .iter()
        .enumerate()
        .map(|(i, b)| [i as f64, get(b)])
        .collect()
}

/// Format a token count compactly (1234 → "1.2k", 1_500_000 → "1.5M").
fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1e3),
        10_000..=999_999 => format!("{:.0}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}
