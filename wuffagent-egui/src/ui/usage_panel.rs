use eframe::egui;
use egui_plot::{GridMark, Legend, Line, Plot};

use super::charts::{fmt_tokens, fmt_usd};
use super::theme::Theme;
use wuffagent_core::config::ModelPrice;
use wuffagent_core::usage::recorder::{UsageEntry, UsageRecorder};
use wuffagent_core::usage::stats::{bucketize, Bucket, Granularity, UsageLogReader};
use wuffagent_core::usage::cost_usd;

/// Aggregation window shown in the chart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    const OPTIONS: [(&'static str, Self); 3] = [
        ("Hour", Self::Hour),
        ("Day", Self::Day),
        ("Week", Self::Week),
    ];

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
    /// 3c: filter selections — "all" = unfiltered, else the exact
    /// agent / model string as recorded on the lines.
    filter_agent: String,
    filter_model: String,
    /// 3c: distinct agent / model values seen in the loaded lines (feed
    /// the filter dropdowns); rebuilt whenever new lines arrive.
    agent_options: Vec<String>,
    model_options: Vec<String>,
    /// 3c: the entries passing the filters (rebuilt only on load/poll or
    /// a filter change — never per frame).
    filtered: Vec<UsageEntry>,
    needs_refilter: bool,
    /// 3c: the config's price table (loaded once; empty = "unpriced").
    prices: Vec<ModelPrice>,
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
            filter_agent: "all".to_string(),
            filter_model: "all".to_string(),
            agent_options: Vec::new(),
            model_options: Vec::new(),
            filtered: Vec::new(),
            needs_refilter: true,
            prices: Vec::new(),
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
                // 3c: the price table, once (empty = "recorded but unpriced").
                self.prices = wuffagent_core::config::Config::load(
                    &wuffagent_core::config::get_config_path(),
                )
                .map(|c| c.model_prices)
                .unwrap_or_default();
            } else {
                let (new_entries, _skipped) = self.reader.poll(&path);
                if !new_entries.is_empty() {
                    self.entries.extend(new_entries);
                }
            }
            self.dirty = false;
            // 3c: rebuild the filter options (distinct values) and drop a
            // stale selection if its value disappeared from the file.
            self.agent_options = distinct_values(&self.entries, |e| e.agent.as_str());
            self.model_options = distinct_values(&self.entries, |e| e.model.as_str());
            if self.filter_agent != "all" && !self.agent_options.contains(&self.filter_agent) {
                self.filter_agent = "all".to_string();
            }
            if self.filter_model != "all" && !self.model_options.contains(&self.filter_model) {
                self.filter_model = "all".to_string();
            }
            self.needs_refilter = true;
        }

        let range = self.range;

        // 3c: apply the filters (only when the lines or a selection changed).
        if self.needs_refilter {
            let fa = self.filter_agent.clone();
            let fm = self.filter_model.clone();
            self.filtered = self
                .entries
                .iter()
                .filter(|e| {
                    (fa == "all" || e.agent == fa) && (fm == "all" || e.model == fm)
                })
                .cloned()
                .collect();
            self.needs_refilter = false;
        }

        let now = chrono::Utc::now();
        let window = bucketize(&self.filtered, range.granularity(), now);

        // 3c: window cost — sum `cost_usd` over the filtered entries inside
        // the SAME local wall-clock window bucketize uses (entries before
        // `window_start` or in the future are excluded). Models missing
        // from the price table count as $0 and are tallied separately.
        let now_local = now.with_timezone(&chrono::Local).naive_local();
        let window_start = range.granularity().window_start(now_local);
        let mut cost = 0.0f64;
        let mut unpriced_tokens: u64 = 0;
        for e in &self.filtered {
            let ts_local = e.ts.with_timezone(&chrono::Local).naive_local();
            if ts_local < window_start || ts_local > now_local {
                continue;
            }
            if self
                .prices
                .iter()
                .any(|p| p.model.eq_ignore_ascii_case(&e.model))
            {
                cost += cost_usd(
                    &self.prices,
                    &e.model,
                    e.prompt_tokens as u64,
                    e.completion_tokens as u64,
                );
            } else {
                unpriced_tokens += e.total_tokens as u64;
            }
        }

        // egui only draws the title-bar ✕ when `open` is provided; a local
        // bool keeps the borrow checker happy (the content closure below
        // borrows `self` for the range selector and data).
        let mut open = self.show_panel;

        egui::Window::new("Token Usage")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_size([620.0, 460.0])
            .min_size([420.0, 340.0])
            .show(ctx, |ui| {
                ui.visuals_mut().panel_fill = theme.surface;

                // Header: title left, window range right.
                ui.horizontal(|ui| {
                    ui.heading("Token Usage");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(range.window_label()).weak());
                    });
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

                // 3c: agent / model filters (the distinct values feed the
                // dropdowns; item 0 is always "all").
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("agent").weak());
                    let mut sel = filter_index(&self.filter_agent, &self.agent_options);
                    let resp = egui::ComboBox::from_id_salt("usage_filter_agent")
                        .show_index(ui, &mut sel, self.agent_options.len() + 1, |i| {
                            filter_option_at(i, &self.agent_options)
                        });
                    if resp.changed() {
                        self.filter_agent = filter_option_at(sel, &self.agent_options);
                        self.needs_refilter = true;
                    }
                    ui.label(egui::RichText::new("model").weak());
                    let mut sel = filter_index(&self.filter_model, &self.model_options);
                    let resp = egui::ComboBox::from_id_salt("usage_filter_model")
                        .show_index(ui, &mut sel, self.model_options.len() + 1, |i| {
                            filter_option_at(i, &self.model_options)
                        });
                    if resp.changed() {
                        self.filter_model = filter_option_at(sel, &self.model_options);
                        self.needs_refilter = true;
                    }
                });
                // The window/cost locals are computed above (outside the
                // closure) from the filtered vec.
                let window = &window;

                // Stat cards above the chart: window totals, calls, tool
                // calls and thinking volume.
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(2.0);
                    stat_card(ui, "input", window.total_prompt_tokens, theme.primary, theme);
                    stat_card(ui, "output", window.total_completion_tokens, theme.accent, theme);
                    stat_card(ui, "total", window.total_tokens, theme.warning, theme);
                    stat_card(
                        ui,
                        "calls",
                        window.calls as u64,
                        theme.text_primary,
                        theme,
                    );
                    stat_card(
                        ui,
                        "tools",
                        window.total_tool_calls as u64,
                        theme.success,
                        theme,
                    );
                    stat_card(
                        ui,
                        "thinking",
                        window.total_thinking_chars,
                        theme.text_secondary,
                        theme,
                    );
                    // 3c: estimated window cost (from the config price table).
                    stat_card_str(
                        ui,
                        "cost",
                        fmt_usd(cost),
                        theme.text_primary,
                        theme,
                    );
                    ui.add_space(2.0);
                });
                // 3c: note the tokens that could not be priced.
                if unpriced_tokens > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "unpriced tokens: {} (model missing from the price table)",
                            fmt_tokens(unpriced_tokens)
                        ))
                        .weak()
                        .small(),
                    );
                }
                ui.add_space(10.0);

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

                    // Cap the chart's height: a Plot greedily takes every
                    // remaining pixel, which would push the hover-details
                    // line and footnote off the bottom of the window.
                    // Reserve ~40px for those (two short text lines).
                    let plot_h = (ui.available_height() - 36.0).max(120.0);
                    let plot_resp = ui.scope(|ui| {
                        ui.set_max_height(plot_h);
                        Plot::new("usage_plot")
                            .min_size(egui::vec2(0.0, 120.0))
                            .legend(Legend::default().position(egui_plot::Corner::LeftTop))
                            .allow_zoom(false)
                            .allow_drag(false)
                            .x_axis_formatter(
                                move |mark: GridMark, _bounds: &std::ops::RangeInclusive<f64>| {
                                    let i = (mark.value.round()).clamp(0.0, (n - 1) as f64) as usize;
                                    buckets[i].start.format("%H:%M").to_string()
                                },
                            )
                            .y_axis_formatter(|mark: GridMark, _bounds: &std::ops::RangeInclusive<f64>| {
                                fmt_tokens(mark.value.max(0.0) as u64)
                            })
                            .show(ui, |pui| {
                                pui.line(
                                    Line::new("input", input_pts)
                                        .color(theme.primary)
                                        .width(1.5),
                                );
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
                            })
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
                                    "{} — in {} · out {} · total {} · {} call{} · {} tool call{} · {} thinking",
                                    stamp,
                                    fmt_tokens(b.prompt_tokens),
                                    fmt_tokens(b.completion_tokens),
                                    fmt_tokens(b.total_tokens),
                                    b.calls,
                                    if b.calls == 1 { "" } else { "s" },
                                    b.tool_calls,
                                    if b.tool_calls == 1 { "" } else { "s" },
                                    fmt_tokens(b.thinking_chars),
                                ))
                                .color(theme.text_primary),
                            );
                        }
                    } else {
                        ui.label(egui::RichText::new("hover the chart for bucket details").weak());
                    }
                }

                // Footnote: what a token roughly is.
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("1 token ≈ 4 characters of text (server-reported counts)")
                        .weak(),
                );
            });

        // The ✕ in the title bar (egui's `open`) may have cleared the flag.
        self.show_panel = open;
    }
}

/// A rounded stat card: small label, big colored value.
fn stat_card(
    ui: &mut egui::Ui,
    label: &str,
    value: u64,
    value_color: egui::Color32,
    theme: &Theme,
) {
    egui::Frame::new()
        .fill(theme.surface_light)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .corner_radius(7.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_min_width(78.0);
            ui.label(egui::RichText::new(label).weak().size(10.5));
            ui.add_space(1.0);
            ui.label(
                egui::RichText::new(fmt_tokens(value))
                    .strong()
                    .size(16.0)
                    .color(value_color),
            );
        });
    ui.add_space(7.0);
}

/// Build one chart series: x = bucket index, y = `get(bucket)`.
fn series_pts(buckets: &[Bucket], get: impl Fn(&Bucket) -> f64) -> Vec<[f64; 2]> {
    buckets
        .iter()
        .enumerate()
        .map(|(i, b)| [i as f64, get(b)])
        .collect()
}

/// 3c: a stat card with an arbitrary text value (the `stat_card` twin for
/// non-token values like the USD cost).
fn stat_card_str(
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
            ui.set_min_width(78.0);
            ui.label(egui::RichText::new(label).weak().size(10.5));
            ui.add_space(1.0);
            ui.label(
                egui::RichText::new(value)
                    .strong()
                    .size(16.0)
                    .color(value_color),
            );
        });
    ui.add_space(7.0);
}

/// 3c: the distinct non-empty values of one `UsageEntry` field, sorted —
/// the options for the filter dropdowns.
fn distinct_values(entries: &[UsageEntry], get: impl Fn(&UsageEntry) -> &str) -> Vec<String> {
    let mut out: Vec<String> = entries
        .iter()
        .map(|e| get(e).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 3c: dropdown index for a selection (0 = "all"; unknown → 0).
fn filter_index(sel: &str, options: &[String]) -> usize {
    if sel == "all" {
        0
    } else {
        options.iter().position(|o| o == sel).map(|p| p + 1).unwrap_or(0)
    }
}

/// 3c: the selection for a dropdown index (0 = "all").
fn filter_option_at(i: usize, options: &[String]) -> String {
    if i == 0 {
        "all".to_string()
    } else {
        options[i - 1].clone()
    }
}
