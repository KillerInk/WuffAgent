//! Tool-card rendering: collapsed tool cards, JSON result views,
//! path badges, code blocks. Split out of ui/chat_area.rs (U3).

use eframe::egui;

use crate::ui::state::ChatApp;
use base64::Engine;

use crate::ui::theme::Theme;
use wuffagent_core::types::ChatMessage;

/// Parsed fields of a persisted tool message (for the collapsed card).
struct ToolCardInfo {
    name: String,
    /// One-line preview of the call's arguments (new-format calls);
    /// empty for legacy calls.
    args: String,
    /// One-line result summary (success) or error text (failure).
    summary: String,
    is_error: bool,
    duration_ms: Option<u64>,
    raw_result: String,
    /// `data:` URI of an image to render inside the card (show_image results).
    image_uri: Option<String>,
}

/// Per-Context cache of tool-result JSON parses (egui temp data — cleared
/// with the context). An expanded tool card re-ran `serde_json::from_str` on
/// the (potentially multi-MB) result every frame; the result text is
/// immutable per message, so parse once and reuse. `None` entries remember
/// non-JSON results so plain-text output doesn't re-fail the parse either.
#[derive(Clone, Default)]
struct ToolJsonCache {
    /// Key: (64-bit hash of the result, byte length) -> parsed result.
    entries: std::collections::HashMap<(u64, u32), std::sync::Arc<Option<serde_json::Value>>>,
}

/// Cache bound in entries; on overflow the whole map is dropped (visible
/// cards re-parse once, memory stays bounded).
const MAX_TOOL_JSON_CACHE_ENTRIES: usize = 128;

/// Per-Context cache of parsed tool-card fields (egui temp data — cleared with
/// the context). See `cached_tool_card` for why the parse is cached.
#[derive(Clone, Default)]
struct ToolCardInfoCache {
    /// Key: (64-bit hash of the content, byte length) -> parsed card.
    entries: std::collections::HashMap<(u64, u32), std::sync::Arc<ToolCardInfo>>,
}

/// Cache bound in entries; on overflow the whole map is dropped (visible
/// cards re-parse once, memory stays bounded).
const MAX_TOOL_CARD_CACHE_ENTRIES: usize = 128;

/// Parsed tool result for `raw`: a cache hit is an O(1) `Arc::clone`, a
/// miss parses once and stores the outcome (`None` = not JSON).
fn parsed_tool_json(ctx: &egui::Context, raw: &str) -> std::sync::Arc<Option<serde_json::Value>> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    raw.hash(&mut h);
    let key = (h.finish(), raw.len() as u32);
    // Raw read (no clone of the whole map): type-keyed at `Id::NULL`.
    let cached = ctx.data(|d| {
        d.get_temp_raw(egui::util::id_type_map::RawKey::new::<ToolJsonCache>(
            egui::Id::NULL,
        ))
        .and_then(|v| v.downcast_ref::<ToolJsonCache>())
        .and_then(|c| c.entries.get(&key))
        .cloned()
    });
    if let Some(hit) = cached {
        return hit;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(raw).ok();
    let arc = std::sync::Arc::new(parsed);
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<ToolJsonCache>(egui::Id::NULL);
        if cache.entries.len() >= MAX_TOOL_JSON_CACHE_ENTRIES {
            cache.entries.clear();
        }
        cache.entries.insert(key, arc.clone());
    });
    arc
}

/// Parse a tool message's content into card fields.
///
/// New format: `header||call_id||result||duration_ms` where header is
/// `🔧 <name>: <args>` (success) or `✗ <name>: <args> — <error>` (failure).
/// Legacy format: `🔧 <name>: <result preview>||call_id||result` or
/// `Tool '<name>' error: <msg>||call_id||`, or a bare result.
fn parse_tool_card(content: &str) -> ToolCardInfo {
    let parts: Vec<&str> = content.splitn(4, "||").collect();
    let header = parts.first().copied().unwrap_or("").trim();
    let raw_result: &str = if parts.len() >= 3 { parts[2] } else { content };
    let duration_ms = parts.get(3).and_then(|d| d.trim().parse::<u64>().ok());

    // Legacy error header: `Tool '<name>' error: <msg>`.
    if let Some(stripped) = header.strip_prefix("Tool '") {
        if let Some((name, rest)) = stripped.split_once('\'') {
            let err = rest
                .trim()
                .strip_prefix("error:")
                .map(|e| e.trim())
                .unwrap_or(rest.trim())
                .to_string();
            return ToolCardInfo {
                name: name.to_string(),
                args: String::new(),
                summary: err,
                is_error: true,
                duration_ms,
                raw_result: raw_result.trim().to_string(),
                image_uri: None,
            };
        }
    }
    // New error header: `✗ <name>: <args> — <error>`.
    if let Some(tail) = header.strip_prefix('✗') {
        let tail = tail.trim();
        if let Some((name, rest)) = tail.split_once(": ") {
            let (args, err) = match rest.rsplit_once(" — ") {
                Some((a, e)) if !e.is_empty() => (a.trim(), e.trim()),
                _ => (rest.trim(), ""),
            };
            return ToolCardInfo {
                name: name.trim().to_string(),
                args: args.to_string(),
                summary: err.to_string(),
                is_error: true,
                duration_ms,
                raw_result: raw_result.trim().to_string(),
                image_uri: None,
            };
        }
    }
    // Success header: `🔧 <name>: <tail>`. For new-format calls the tail is
    // the ARGS preview; for legacy calls it's a result preview (ignored —
    // the summary comes from the result itself, as before).
    let has_header = parts.len() >= 2 && !header.is_empty();
    let name = if has_header {
        let tail = header
            .find(|c: char| c.is_alphanumeric())
            .map(|i| &header[i..])
            .unwrap_or(header);
        let n: String = tail
            .chars()
            .take_while(|c| !matches!(c, ':' | '(' | ' '))
            .collect();
        if n.is_empty() {
            "Tool".to_string()
        } else {
            n
        }
    } else {
        "Tool".to_string()
    };
    // New-format success header tail = args preview (after "name: ").
    let args = if duration_ms.is_some() {
        header
            .find(&format!("{}: ", name))
            .map(|i| header[i + name.len() + 2..].trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    ToolCardInfo {
        name,
        args,
        summary: tool_result_summary(raw_result),
        is_error: false,
        duration_ms,
        raw_result: raw_result.trim().to_string(),
        image_uri: tool_result_image_uri(raw_result),
    }
}

/// Extract a renderable image `data:` URI from a tool result (show_image
/// returns one in its JSON `data_uri` field), if present.
fn tool_result_image_uri(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw.trim()).ok()?;
    value
        .get("data_uri")
        .and_then(|d| d.as_str())
        .filter(|d| d.starts_with("data:image/"))
        .map(str::to_string)
}

/// One-line summary of a tool result shown in the collapsed tool card.
fn tool_result_summary(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "(no output)".to_string();
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // Image result (show_image): name/caption + dimensions, not "N chars".
        if json
            .get("data_uri")
            .and_then(|v| v.as_str())
            .is_some_and(|d| d.starts_with("data:image/"))
        {
            let label = json
                .get("caption")
                .and_then(|v| v.as_str())
                .or_else(|| json.get("path").and_then(|v| v.as_str()))
                .unwrap_or("image");
            let label: String = label.chars().take(64).collect();
            let w = json.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
            let h = json.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
            return format!("🖼 {label} · {w}×{h} px");
        }
        // Shell output: exit status + output volume.
        if let Some(code) = json.get("exit_code").and_then(|v| v.as_u64()) {
            let lines_part = |key: &str| -> Option<String> {
                let n = json
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(|s| s.lines().count())?;
                if n == 0 {
                    None
                } else {
                    Some(format!(
                        "{} {} line{}",
                        n,
                        key,
                        if n == 1 { "" } else { "s" }
                    ))
                }
            };
            let mut detail = lines_part("stdout");
            if let Some(e) = lines_part("stderr") {
                detail = Some(match detail {
                    Some(d) => format!("{} · {}", d, e),
                    None => e,
                });
            }
            let detail = detail.map(|d| format!(" · {}", d)).unwrap_or_default();
            if code == 0 {
                return format!("✓ exit 0{}", detail);
            }
            return format!("✗ exit {}{}", code, detail);
        }
        if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
            if let Some(entries) = json.get("entries").and_then(|v| v.as_array()) {
                return format!("{} · {} entries", path, entries.len());
            }
            if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                let lines = json
                    .get("total_lines")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or_else(|| content.lines().count());
                return format!("{} · {} lines", path, lines);
            }
            if let Some(bytes) = json.get("bytes_written").and_then(|v| v.as_u64()) {
                return format!("✓ {} bytes written", bytes);
            }
            if json.get("success").and_then(|v| v.as_bool()) == Some(true) {
                return "✓ done".to_string();
            }
            return path.to_string();
        }
        if let Some(matches) = json.get("matches").and_then(|v| v.as_array()) {
            let files = json
                .get("files_searched")
                .and_then(|v| v.as_u64())
                .map(|f| format!(" in {} files", f))
                .unwrap_or_default();
            let plural = if matches.len() == 1 { "" } else { "es" };
            return format!("{} match{}{}", matches.len(), plural, files);
        }
        if let Some(expr) = json.get("expression").and_then(|v| v.as_str()) {
            if let Some(result) = json.get("result").and_then(|v| v.as_f64()) {
                return format!("{} = {}", expr, result);
            }
        }
        return format!("{} chars", trimmed.len());
    }
    // Plain text: first line, truncated.
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    let first: String = first_line.chars().take(72).collect();
    if first_line.chars().count() > 72 {
        format!("{}…", first)
    } else if trimmed.lines().count() > 1 {
        format!("{} … ({} lines)", first, trimmed.lines().count())
    } else {
        first
    }
}

/// Decode a `data:[<mime>];base64,<payload>` URI into raw image bytes.
fn data_uri_to_bytes(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if !meta.to_ascii_lowercase().ends_with(";base64") {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
}

impl ChatApp {
    /// Collapsed row: status icon, tool icon + name, the call's args preview
    /// (what it did), a result/error summary, a duration chip, and the time.
    /// Clicking the header row expands the full result detail (right-click
    /// opens the delete menu). The card is indented to sit under the AI
    /// message column and has no avatar, keeping tool chatter visually quiet
    /// compared to normal messages.
    pub(super) fn draw_tool_card(
        &mut self,
        ui: &mut egui::Ui,
        message: &ChatMessage,
        index: usize,
        theme: &Theme,
    ) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Indent under the AI bubble (28px avatar + 8px gap).
            ui.add_space(36.0);
            // The card Frame's content ui inherits this ui's layout (egui 0.36
            // Frame has no layout option of its own), so without this the card
            // body would be laid out HORIZONTALLY: the header row (stretched to
            // full width by the right-aligned timestamp) consumes the whole row
            // and the expanded content is squeezed into a ~0px sliver, wrapping
            // one character per line. Force a vertical layout for the card body.
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                let is_expanded = self
                    .sessions
                    .selected_session_id
                    .as_ref()
                    .map(|sid| {
                        self.sessions
                            .session_store
                            .get(sid)
                            .map(|r| r.chat_state.expanded_messages.contains(&index))
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);

                // Content is "header||call_id||result[||duration_ms]" — or a
                // bare result for messages loaded from older sessions.
                let card = Self::cached_tool_card(ui.ctx(), &message.content);
                let name = card.name.clone();
                let summary = card.summary.clone();
                let is_error = card.is_error;
                let args = card.args.clone();
                let duration_ms = card.duration_ms;
                let image_uri = card.image_uri.clone();
                let ts = wuffagent_core::types::timestamp_time(&message.timestamp);

                egui::Frame::NONE
                    .fill(theme.surface)
                    .stroke(egui::Stroke::new(1.0, theme.bubble_border))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.take_available_width();
                        // Header row: chevron + tool icon+name + args preview +
                        // result/error summary + duration + timestamp. The whole
                        // row is clickable (expand/collapse) and
                        // right-clickable (delete).
                        let row = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let chev = if is_expanded { "▾" } else { "▸" };
                            ui.label(
                                egui::RichText::new(chev)
                                    .color(theme.text_dim)
                                    .size(9.0)
                                    .monospace(),
                            );
                            ui.label(
                                egui::RichText::new(format!("{} {}", super::tool_icon(&name), name))
                                    .color(if is_error {
                                        theme.warning
                                    } else {
                                        theme.accent
                                    })
                                    .strong()
                                    .size(11.5),
                            );
                            // What the call did (new-format calls); legacy calls
                            // have no args preview and just show the summary.
                            // A wrapping label in a horizontal row claims all
                            // remaining width, so cap it — reserving room for
                            // the summary and the duration/timestamp cluster.
                            if !args.is_empty() {
                                let avail = ui.available_width();
                                let args_max = (avail - 160.0).clamp(80.0, 420.0);
                                ui.scope(|ui| {
                                    ui.set_max_width(args_max);
                                    ui.add(super::breaking_label(
                                        &args,
                                        egui::FontId::monospace(10.5),
                                        theme.text_dim,
                                        false,
                                    ));
                                });
                            }
                            let summary_color = if is_error {
                                theme.warning
                            } else {
                                theme.text_dim
                            };
                            // Wrap the summary within the leftover header width (reserving
                            // 200px for the duration/timestamp cluster) so a long summary
                            // wraps instead of overflowing the now full-width card.
                            if !summary.is_empty() {
                                let summary_max = (ui.available_width() - 200.0).max(120.0);
                                ui.scope(|ui| {
                                    ui.set_max_width(summary_max);
                                    ui.add(super::breaking_label(
                                        if is_error {
                                            format!("✗ {}", summary)
                                        } else {
                                            summary.clone()
                                        },
                                        egui::FontId::proportional(10.5),
                                        summary_color,
                                        false,
                                    ));
                                });
                            }
                            // Right cluster: duration chip + timestamp.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(ms) = duration_ms {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "· {}",
                                                super::format_duration(ms)
                                            ))
                                            .color(theme.text_dim)
                                            .size(9.5),
                                        );
                                    }
                                    if !ts.is_empty() {
                                        ui.label(
                                            egui::RichText::new(ts).color(theme.text_dim).size(9.5),
                                        );
                                    }
                                },
                            );
                        });
                        // The layout's own response only tracks hover, so
                        // register an explicit click interaction over the row.
                        let row_click = ui.interact(
                            row.response.rect,
                            ui.id().with("tool_toggle").with(index),
                            egui::Sense::click(),
                        );
                        if row_click.hovered() {
                            // Subtle highlight signals the row is clickable.
                            ui.painter().rect(
                                row.response.rect,
                                6.0,
                                theme.hover_bg,
                                egui::Stroke::NONE,
                                egui::StrokeKind::Middle,
                            );
                        }
                        if row_click.clicked() {
                            if let Some(sid) = &self.sessions.selected_session_id {
                                if let Some(runtime) = self.sessions.session_store.get_mut(sid) {
                                    if is_expanded {
                                        runtime
                                            .chat_state
                                            .expanded_messages
                                            .retain(|&i| i != index);
                                    } else {
                                        runtime.chat_state.expanded_messages.push(index);
                                    }
                                }
                            }
                        }
                        if row_click.secondary_clicked() {
                            row_click.context_menu(|menu_ui| {
                                menu_ui.set_min_width(120.0);
                                if menu_ui.button("Delete").clicked() {
                                    super::bubbles::delete_message(&mut self.sessions, index);
                                }
                            });
                        }
                        if is_expanded {
                            ui.add_space(6.0);
                            let min = ui.cursor().min;
                            ui.painter().hline(
                                min.x..=min.x + ui.available_width(),
                                min.y,
                                egui::Stroke::new(1.0, theme.divider),
                            );
                            ui.add_space(6.0);
                        } else if let Some(uri) = &image_uri {
                            // show_image: the whole point is the picture — render
                            // it right in the collapsed card (the expanded view
                            // shows the image plus the metadata JSON fields).
                            ui.add_space(6.0);
                            draw_data_uri_image(ui, uri, 260.0, theme);
                        }
                        if is_expanded {
                        if card.raw_result.trim().is_empty() {
                            ui.label(
                                egui::RichText::new("(no output)")
                                    .color(theme.text_dim)
                                    .italics()
                                    .size(11.0),
                            );
                        } else {
                            // Parse once per result body (cached in the egui
                            // context): re-parsing multi-MB results ran on
                            // every frame for each expanded card.
                            let parsed = parsed_tool_json(ui.ctx(), card.raw_result.as_str());
                            match &*parsed {
                                Some(json) => {
                                    super::tool_json::draw_tool_json_result(
                                        ui,
                                        json,
                                        card.raw_result.as_str(),
                                        theme,
                                    );
                                }
                                None => {
                                    draw_tool_plain_result(ui, card.raw_result.as_str(), theme);
                                }
                            }
                        }
                        }
                    });
            });
        });
    }

    /// Parsed tool-card fields for `content`, cached per content in the egui
    /// context (same pattern as `parsed_tool_json` above). Immediate mode
    /// redraws every visible card each frame, but a committed tool result never
    /// changes. The parse is the expensive part: it copies `raw_result`, then
    /// runs two full `serde_json` parses (a `data:`-URI scan for the header + a
    /// JSON summary), so caching it avoids ~2 JSON parses + 1 large copy per
    /// card per frame.
    fn cached_tool_card(ctx: &egui::Context, content: &str) -> std::sync::Arc<ToolCardInfo> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut h);
        let key = (h.finish(), content.len() as u32);
        // Raw read (no clone of the whole map): type-keyed at `Id::NULL`.
        let cached = ctx.data(|d| {
            d.get_temp_raw(egui::util::id_type_map::RawKey::new::<ToolCardInfoCache>(
                egui::Id::NULL,
            ))
            .and_then(|v| v.downcast_ref::<ToolCardInfoCache>())
            .and_then(|c| c.entries.get(&key))
            .cloned()
        });
        if let Some(hit) = cached {
            return hit;
        }
        let info = parse_tool_card(content);
        let arc = std::sync::Arc::new(info);
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<ToolCardInfoCache>(egui::Id::NULL);
            if cache.entries.len() >= MAX_TOOL_CARD_CACHE_ENTRIES {
                cache.entries.clear();
            }
            cache.entries.insert(key, arc.clone());
        });
        arc
    }
}

/// Draw a clickable file path chip.
pub(super) fn draw_tool_path_badge(ui: &mut egui::Ui, path: &str, theme: &Theme) {
        // Elide the displayed path when it cannot fit (buttons can't
        // wrap); the click handler still uses the full path.
        let font_id = egui::FontId::new(10.0, egui::FontFamily::Monospace);
        let per_char = super::char_width(ui, &font_id);
        // Reserve room for chip padding.
        let budget = (ui.available_width() - 28.0).max(40.0);
        let max_chars = ((budget * 0.96) / per_char).floor() as usize;
        let path_chars: Vec<char> = path.chars().collect();
        let display = if path_chars.len() > max_chars && max_chars >= 2 {
            let mut s = String::with_capacity(max_chars);
            s.push('…');
            s.extend(path_chars.iter().skip(path_chars.len() - (max_chars - 1)));
            s
        } else {
            path.to_string()
        };
        // Path as a clickable chip
        let path_btn = egui::Button::new(
            egui::RichText::new(display)
                .color(theme.badge_text)
                .size(10.0)
                .monospace(),
        )
        .fill(theme.badge_bg)
        .corner_radius(5)
        .min_size(egui::vec2(0.0, 18.0));
        if ui.add(path_btn).clicked() {
            // Open parent directory in explorer
            if let Some(parent) = std::path::Path::new(path).parent() {
                let parent_str = parent.to_string_lossy().to_string();
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("explorer")
                        .args(["/select,", &parent_str])
                        .spawn();
                }
                #[cfg(unix)]
                {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(parent_str)
                        .spawn();
                }
            }
        }
    }

    /// Render a plain (non-JSON) tool result as a monospace code block.
    /// Long text (e.g. read_file's raw output) scrolls inside a capped
    /// height, mirroring the JSON file-read card.
pub(super) fn draw_tool_plain_result(ui: &mut egui::Ui, text: &str, theme: &Theme) {
        super::code_block(ui, theme, |ui| {
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(super::breaking_label(
                        text,
                        egui::FontId::monospace(13.0),
                        theme.code_text,
                        false,
                    ));
                });
        });
    }

    /// Render an image from a `data:image/...;base64,` URI (as returned by
    /// the show_image tool), scaled to fit the available width / max height.
    ///
    /// The decoded bytes are hashed into the texture URI because egui's bytes
    /// loader keeps the FIRST payload stored per URI (a fixed URI would show
    /// a stale image — same reasoning as the chat-input attach flow).
pub(super) fn draw_data_uri_image(
    ui: &mut egui::Ui,
    uri: &str,
    max_height: f32,
    theme: &Theme,
) {
        let Some(bytes) = data_uri_to_bytes(uri) else {
            ui.label(
                egui::RichText::new("(could not decode image)")
                    .color(theme.text_dim)
                    .size(10.5)
                    .italics(),
            );
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hasher::write(&mut hasher, &bytes);
        let hash = std::hash::Hasher::finish(&mut hasher);
        let img = egui::Image::from_bytes(format!("show_image_{hash:016x}.jpg"), bytes);
        let max_width = (ui.available_width() - 8.0).max(60.0);
        ui.add(img.max_size(egui::Vec2::new(max_width, max_height)));
    }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_empty_is_placeholder() {
        assert_eq!(tool_result_summary(""), "(no output)");
        assert_eq!(tool_result_summary("   \n  "), "(no output)");
    }

    #[test]
    fn summary_shell_success_shows_exit_and_line_count() {
        assert_eq!(
            tool_result_summary(r#"{"exit_code":0,"stdout":"hello\n"}"#),
            "✓ exit 0 · 1 stdout line"
        );
    }

    #[test]
    fn summary_shell_failure_shows_exit_and_stderr() {
        assert_eq!(
            tool_result_summary(r#"{"exit_code":1,"stderr":"boom\n"}"#),
            "✗ exit 1 · 1 stderr line"
        );
    }

    #[test]
    fn summary_shell_both_streams_joined() {
        let s = tool_result_summary(r#"{"exit_code":0,"stdout":"a\nb\n","stderr":"w\n"}"#);
        assert_eq!(s, "✓ exit 0 · 2 stdout lines · 1 stderr line");
    }

    #[test]
    fn summary_calculation_shows_expression_and_result() {
        assert_eq!(
            tool_result_summary(r#"{"expression":"2+3","result":5}"#),
            "2+3 = 5"
        );
    }

    #[test]
    fn summary_image_shows_label_and_dimensions() {
        assert_eq!(
            tool_result_summary(
                r#"{"data_uri":"data:image/png;base64,AAA","caption":"pic","width":100,"height":50}"#
            ),
            "🖼 pic · 100×50 px"
        );
    }

    #[test]
    fn summary_plain_single_line_unchanged() {
        assert_eq!(tool_result_summary("hello"), "hello");
    }

    #[test]
    fn summary_plain_long_line_truncated() {
        let s = tool_result_summary(&"a".repeat(80));
        assert!(s.ends_with('…'));
        // 72 kept chars + ellipsis
        assert_eq!(s.chars().count(), 73);
    }

    #[test]
    fn summary_plain_multi_line_appends_count() {
        assert_eq!(
            tool_result_summary("line1\nline2\nline3"),
            "line1 … (3 lines)"
        );
    }

    #[test]
    fn image_uri_extracted_from_json() {
        assert_eq!(
            tool_result_image_uri(r#"{"data_uri":"data:image/png;base64,AAA"}"#),
            Some("data:image/png;base64,AAA".to_string())
        );
    }

    #[test]
    fn image_uri_none_when_missing_or_not_image() {
        assert_eq!(tool_result_image_uri(r#"{"foo":1}"#), None);
        assert_eq!(tool_result_image_uri(r#"{"data_uri":"http://x"}"#), None);
        assert_eq!(tool_result_image_uri("not json"), None);
    }

    #[test]
    fn parse_new_format_success() {
        let c = "🔧 shell: echo hi||call123||{\"exit_code\":0}||42";
        let info = parse_tool_card(c);
        assert_eq!(info.name, "shell");
        assert_eq!(info.args, "echo hi");
        assert!(!info.is_error);
        assert_eq!(info.duration_ms, Some(42));
        assert_eq!(info.summary, "✓ exit 0");
        assert_eq!(info.raw_result, "{\"exit_code\":0}");
    }

    #[test]
    fn parse_new_format_error() {
        let c = "✗ read_file: path x — not found||c1||||10";
        let info = parse_tool_card(c);
        assert!(info.is_error);
        assert_eq!(info.name, "read_file");
        assert_eq!(info.args, "path x");
        assert_eq!(info.summary, "not found");
        assert_eq!(info.duration_ms, Some(10));
    }

    #[test]
    fn parse_legacy_error() {
        let c = "Tool 'write_file' error: disk full||c1||";
        let info = parse_tool_card(c);
        assert!(info.is_error);
        assert_eq!(info.name, "write_file");
        assert_eq!(info.args, "");
        assert_eq!(info.summary, "disk full");
        assert_eq!(info.duration_ms, None);
    }

    #[test]
    fn parse_bare_result() {
        let info = parse_tool_card("just some plain output");
        assert_eq!(info.name, "Tool");
        assert!(!info.is_error);
        assert_eq!(info.summary, "just some plain output");
        assert_eq!(info.duration_ms, None);
    }

    #[test]
    fn data_uri_decodes_base64() {
        assert_eq!(
            data_uri_to_bytes("data:image/png;base64,SGVsbG8="),
            Some(b"Hello".to_vec())
        );
    }

    #[test]
    fn data_uri_rejects_non_base64_and_garbage() {
        assert_eq!(data_uri_to_bytes("data:image/png,SGVsbG8="), None);
        assert_eq!(data_uri_to_bytes("nope"), None);
        assert_eq!(data_uri_to_bytes("data:image/png;base64,!!!"), None);
    }
}
