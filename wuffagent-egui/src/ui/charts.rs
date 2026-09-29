//! 3a: shared chart helpers for the egui panels (token-usage panel + fleet
//! dashboard): compact number formatting, index-series building, and a
//! trend Plot (bars + lines) with capped height, left-top legend, and no
//! zoom/drag. Both panels draw with identical axes so a series reads the
//! same wherever it appears.
use eframe::egui;
use egui_plot::{Bar, BarChart, GridMark, Legend, Line, Plot};

/// Format a token count compactly (1234 → "1.2k", 1_500_000 → "1.5M").
pub fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1e3),
        10_000..=999_999 => format!("{:.0}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// Format a USD amount compactly (0.042 → "$0.04", 12.34 → "$12.34",
/// 0 → "—").
pub fn fmt_usd(v: f64) -> String {
    if v <= f64::EPSILON {
        "—".to_string()
    } else {
        format!("${v:.2}")
    }
}

/// Build one chart series indexed by point position: `values[i]` → `[i, values[i]]`.
pub fn index_series(values: &[f64]) -> Vec<[f64; 2]> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| [i as f64, *v])
        .collect()
}

/// Draw a trend Plot: optional bar series + line series.
///
/// - `n`: number of x points (tick indices are clamped to `0..n-1`).
/// - `height`: max plot height in px (a Plot greedily takes the remaining
///   vertical space; cap it so content below stays visible).
/// - `bars`: bar series (points + color) or `None`.
/// - `lines`: line series (name/color/width come from each `Line`).
/// - `x_label`/`y_label`: axis tick formatters.
/// - `pointer_out`: set to the pointer's plot-x (data coordinate) while
///   the pointer is inside the plot — the caller shows hover details for
///   that bucket.
///
/// Returns the plot's response.
pub fn trend_plot(
    ui: &mut egui::Ui,
    id: &str,
    n: usize,
    height: f32,
    bars: Option<(Vec<[f64; 2]>, egui::Color32)>,
    lines: Vec<Line>,
    x_label: impl Fn(usize) -> String + 'static,
    y_label: impl Fn(f64) -> String + 'static,
    pointer_out: &mut Option<f64>,
) -> egui::Response {
    ui.scope(|ui| {
        ui.set_max_height(height);
        Plot::new(id)
            .min_size(egui::vec2(0.0, 120.0))
            .legend(Legend::default().position(egui_plot::Corner::LeftTop))
            .allow_zoom(false)
            .allow_drag(false)
            .x_axis_formatter(move |mark: GridMark, _bounds: &std::ops::RangeInclusive<f64>| {
                let i = (mark.value.round()).clamp(0.0, (n.saturating_sub(1)) as f64) as usize;
                x_label(i)
            })
            .y_axis_formatter(|mark: GridMark, _bounds: &std::ops::RangeInclusive<f64>| {
                y_label(mark.value.max(0.0))
            })
            .show(ui, |pui| {
                if let Some((pts, color)) = bars {
                    let bars: Vec<Bar> = pts
                        .into_iter()
                        .map(|p| Bar::new(p[0], p[1]))
                        .collect();
                    pui.bar_chart(BarChart::new("bars", bars).color(color).width(0.6));
                }
                for line in lines {
                    pui.line(line);
                }
                if let Some(p) = pui.pointer_coordinate() {
                    *pointer_out = Some(p.x);
                }
            })
    })
    .response
}

/// Format a UTC timestamp as a relative "ago" string ("42m ago", "3h ago",
/// "12d ago"); `None` → "never".
pub fn ago(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    let Some(ts) = ts else {
        return "never".to_string();
    };
    let secs = chrono::Utc::now().signed_duration_since(ts).num_seconds().max(0);
    if secs < 90 {
        format!("{secs}s ago")
    } else if secs < 90 * 60 {
        format!("{}m ago", secs / 60)
    } else if secs < 48 * 3600 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}
