//! Tool-card rendering: collapsed tool cards, JSON result views,
//! path badges, code blocks. Split out of ui/chat_area.rs (U3).

use eframe::egui;

use crate::ui::state::ChatApp;
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
}

impl ChatApp {
    /// Collapsed row: status icon, tool icon + name, the call's args preview
    /// (what it did), a result/error summary, a duration chip, and the time.
    /// Clicking the header row expands the full result detail (right-click
    /// opens the delete menu). The card is indented to sit under the AI
    /// message column and has no avatar, keeping tool chatter visually quiet
    /// compared to normal messages.
    pub(super) fn draw_tool_card(&mut self, ui: &mut egui::Ui, message: &ChatMessage, index: usize, theme: &Theme) {
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
                let is_expanded = self.selected_session_id.as_ref().map(|sid| {
                    self.session_store.get(sid).map(|r| r.chat_state.expanded_messages.contains(&index)).unwrap_or(false)
                }).unwrap_or(false);

                // Content is "header||call_id||result[||duration_ms]" — or a
                // bare result for messages loaded from older sessions.
                let card = Self::parse_tool_card(&message.content);
                let name = card.name.clone();
                let summary = card.summary.clone();
                let is_error = card.is_error;
                let raw_result = card.raw_result.clone();
                let args = card.args.clone();
                let duration_ms = card.duration_ms;
                let ts = wuffagent_core::types::timestamp_time(&message.timestamp);

                egui::Frame::NONE
                    .fill(theme.surface)
                    .stroke(egui::Stroke::new(1.0, theme.bubble_border))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        // Header row: chevron + tool icon+name + args preview +
                        // result/error summary + duration + timestamp. The whole
                        // row is clickable (expand/collapse) and
                        // right-clickable (delete).
                        let row = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let chev = if is_expanded { "▾" } else { "▸" };
                            ui.label(egui::RichText::new(chev)
                                .color(theme.text_dim).size(9.0).monospace());
                            ui.label(egui::RichText::new(format!("{} {}", Self::tool_icon(&name), name))
                                .color(if is_error { theme.warning } else { theme.accent })
                                .strong()
                                .size(11.5));
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
                                    ui.add(Self::breaking_label(
                                        &args,
                                        egui::FontId::monospace(10.5),
                                        theme.text_dim,
                                        false,
                                    ));
                                });
                            }
                            let summary_color = if is_error { theme.warning } else { theme.text_dim };
                            ui.label(egui::RichText::new(if is_error && !summary.is_empty() {
                                format!("✗ {}", summary)
                            } else {
                                summary.clone()
                            })
                            .color(summary_color).size(10.5));
                            // Right cluster: duration chip + timestamp.
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if let Some(ms) = duration_ms {
                                    ui.label(egui::RichText::new(format!("· {}", Self::format_duration(ms)))
                                        .color(theme.text_dim).size(9.5));
                                }
                                if !ts.is_empty() {
                                    ui.label(egui::RichText::new(ts)
                                        .color(theme.text_dim).size(9.5));
                                }
                            });
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
                            if let Some(sid) = &self.selected_session_id {
                                if let Some(runtime) = self.session_store.get_mut(sid) {
                                    if is_expanded {
                                        runtime.chat_state.expanded_messages.retain(|&i| i != index);
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
                                    self.delete_message(index);
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
                            if raw_result.trim().is_empty() {
                                ui.label(egui::RichText::new("(no output)")
                                    .color(theme.text_dim).italics().size(11.0));
                            } else if let Ok(json) = serde_json::from_str::<serde_json::Value>(raw_result.as_str()) {
                                self.draw_tool_json_result(ui, &json, raw_result.as_str(), theme);
                            } else {
                                self.draw_tool_plain_result(ui, raw_result.as_str(), theme);
                            }
                        }
                    });
            });
        });
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
            summary: Self::tool_result_summary(raw_result),
            is_error: false,
            duration_ms,
            raw_result: raw_result.trim().to_string(),
        }
    }

    /// One-line summary of a tool result shown in the collapsed tool card.
    fn tool_result_summary(raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return "(no output)".to_string();
        }
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            // Shell output: exit status + output volume.
            if let Some(code) = json.get("exit_code").and_then(|v| v.as_u64()) {
                let lines_part = |key: &str| -> Option<String> {
                    let n = json.get(key).and_then(|v| v.as_str()).map(|s| s.lines().count())?;
                    if n == 0 {
                        None
                    } else {
                        Some(format!("{} {} line{}", n, key, if n == 1 { "" } else { "s" }))
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

    /// Draw a clickable file path chip.
    pub(super) fn draw_tool_path_badge(&self, ui: &mut egui::Ui, path: &str, theme: &Theme) {
        // Elide the displayed path when it cannot fit (buttons can't
        // wrap); the click handler still uses the full path.
        let font_id = egui::FontId::new(10.0, egui::FontFamily::Monospace);
        let per_char = Self::char_width(ui, &font_id);
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
        let path_btn = egui::Button::new(egui::RichText::new(display)
            .color(theme.badge_text)
            .size(10.0)
            .monospace())
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
    pub(super) fn draw_tool_plain_result(&self, ui: &mut egui::Ui, text: &str, theme: &Theme) {
        Self::code_block(ui, theme, |ui| {
            ui.add(Self::breaking_label(
                text,
                egui::FontId::monospace(13.0),
                theme.code_text,
                false,
            ));
        });
    }

    /// Themed code block: theme background, 1px border, uniform padding.
    pub(super) fn code_block(ui: &mut egui::Ui, theme: &Theme, add_contents: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::NONE
            .fill(theme.code_bg)
            .stroke(egui::Stroke::new(1.0, theme.code_border))
            .corner_radius(6)
            .inner_margin(egui::Margin::same(8))
            .show(ui, add_contents);
    }
}
