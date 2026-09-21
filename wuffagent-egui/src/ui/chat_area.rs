use eframe::egui;

use super::state::ChatApp;
use crate::types::{ChatMessage, MessageKind};
use super::theme::Theme;

// ── TEMPORARY layout debugging (set WUFF_LAYOUT_DBG=1 to enable) ────────
static LAYOUT_DBG_FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn layout_dbg_enabled() -> bool {
    std::env::var_os("WUFF_LAYOUT_DBG").is_some()
        && (2..7).contains(&(LAYOUT_DBG_FRAME.load(std::sync::atomic::Ordering::Relaxed) % 1000))
}
// ────────────────────────────────────────────────────────────────────────

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
    /// Threshold in pixels to consider the user as "at bottom"
    const SCROLL_BOTTOM_THRESHOLD: f32 = 10.0;

    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        LAYOUT_DBG_FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let theme = Theme::from_name(&self.config.theme);

        // Get the current session's chat state, or show empty state
        // Rebuild the shared message snapshot only when it is stale: the selected
        // session changed, an in-place edit set `display_dirty`, or the message
        // count changed (append/remove). Otherwise reuse it — a per-frame redraw is
        // then an O(1) `Arc::clone` rather than a deep clone of every message
        // (which copies large tool outputs + base64 images and is the main lag).
        let selected = self.selected_session_id.clone();
        let current_len = self
            .selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .map(|r| r.chat_state.messages.len())
            .unwrap_or(0);
        if selected != self.snapshot_session
            || self.display_dirty
            || current_len != self.snapshot_len
        {
            let msgs = self
                .selected_session_id
                .as_ref()
                .and_then(|sid| self.session_store.get(sid))
                .map(|r| r.chat_state.messages.clone())
                .unwrap_or_default();
            self.snapshot_len = msgs.len();
            self.snapshot_session = selected;
            self.display_snapshot = std::sync::Arc::new(msgs);
            self.display_dirty = false;
        }
        let messages = self.display_snapshot.clone();

        // Show pending error as a subtle red-tinted card (from the current session)
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get(sid) {
                if let Some(err) = &runtime.chat_state.pending_error {
                    egui::Frame::NONE
                        .fill(theme.error.linear_multiply(0.12))
                        .stroke(egui::Stroke::new(1.0, theme.error.linear_multiply(0.35)))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.add(Self::breaking_label(
                                format!("⚠  {}", err),
                                egui::FontId::proportional(12.0),
                                theme.error,
                                false,
                            ));
                        });
                    ui.add_space(10.0);
                }
            }
        }

        // Snapshot streaming state up front so the scroll closure can call
        // `&mut self` helpers without holding an immutable borrow of the store.
        let (streaming, is_streaming) = self
            .selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .filter(|r| r.chat_state.is_generating)
            .map(|r| {
                (
                    (r.chat_state.current_thinking.clone(), r.chat_state.stream_buffer.clone()),
                    true,
                )
            })
            .unwrap_or_default();

        // Live tool cards (small: name + args preview + output tail) — snapshotted
        // for the same reason as the streaming state above.
        let active_tools: Vec<crate::sessions::ActiveTool> = self
            .selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .map(|r| r.chat_state.active_tools.clone())
            .unwrap_or_default();

        // Stick to bottom when the user is already there or forced the button.
        let scroll_output = egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, true])
            .stick_to_bottom(
                self.selected_session_id.as_ref().map(|sid| {
                    self.session_store.get(sid).map(|r| r.chat_state.scroll_to_bottom_requested).unwrap_or(false)
                }).unwrap_or(false) ||
                self.selected_session_id.as_ref().map(|sid| {
                    self.session_store.get(sid).map(|r| r.chat_state.at_bottom).unwrap_or(false)
                }).unwrap_or(false)
            )
            .show(ui, |ui| {
                // Centered content column with a max width so very wide
                // windows don't stretch bubbles edge to edge.
                ui.horizontal_centered(|ui| {
                    ui.scope(|ui| {
                        ui.set_max_width(880.0);
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                            if layout_dbg_enabled() {
                                eprintln!("[ldbg] list avail_w={:.1} max_rect={:?}", ui.available_width(), ui.max_rect());
                            }
                            if messages.is_empty() && !is_streaming {
                                Self::draw_empty_state(ui, &theme);
                                return;
                            }
                            let mut prev_day: Option<&str> = None;
                            for (i, msg) in messages.iter().enumerate() {
                                if let Some(day) = crate::types::timestamp_day(&msg.timestamp) {
                                    if prev_day != Some(day) {
                                        if prev_day.is_some() {
                                            Self::draw_date_separator(ui, day, &theme);
                                        }
                                        prev_day = Some(day);
                                    }
                                }
                                self.draw_message(ui, msg, i, &theme);
                            }
                            // Draw streaming line (values snapshotted before the scroll area).
                            // Only while a response is actually in flight — otherwise the
                            // empty-buffer branch would draw a stray "AI:" + spinner.
                            if is_streaming {
                                self.draw_streaming_line(ui, &theme, &streaming);
                            }
                            // Live tool cards for calls that are executing right now.
                            for tool in &active_tools {
                                self.draw_active_tool_card(ui, tool, &theme);
                            }
                        });
                        ui.add_space(12.0);
                    });
                });
            });

        // Update scroll state for the current session.
        // Compute `at_bottom` (immutable self borrow) before mutating the store.
        let at_bottom = self.is_at_bottom_from_output(&scroll_output);
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                runtime.chat_state.scroll_to_bottom_requested = false;
                runtime.chat_state.at_bottom = at_bottom;
                
                // Update button visibility and opacity
                if runtime.chat_state.at_bottom {
                    runtime.chat_state.button_opacity = (runtime.chat_state.button_opacity * 0.85).max(0.0);
                    if runtime.chat_state.button_opacity < 0.01 {
                        runtime.chat_state.button_visible = false;
                    }
                } else {
                    runtime.chat_state.button_visible = true;
                    runtime.chat_state.button_opacity = (runtime.chat_state.button_opacity + 0.12).min(1.0);
                }
            }
        }

        // Show scroll-to-bottom button when not at bottom and opacity > 0.
        // Snapshot the opacity first so we can call an `&mut self` helper.
        let (button_visible, button_opacity) = self
            .selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .map(|r| (r.chat_state.button_visible, r.chat_state.button_opacity))
            .unwrap_or((false, 0.0));
        if button_visible && button_opacity > 0.01 {
            self.draw_scroll_to_bottom_button(ui, &theme, button_opacity);
        }
    }

    /// Placeholder shown for sessions without any messages yet.
    fn draw_empty_state(ui: &mut egui::Ui, theme: &Theme) {
        // Nudge down toward the vertical middle of the visible area.
        let top_padding = (ui.available_height() - 160.0) * 0.35;
        ui.add_space(top_padding.max(24.0));
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("🐾").size(36.0));
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Start a conversation")
                .color(theme.text_primary)
                .strong()
                .size(16.0));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Ask a question or give the agent a task.")
                .color(theme.text_dim)
                .size(12.0));
        });
    }

    /// Centered divider ("Today" / "Yesterday" / date) between message groups
    /// that cross a day boundary.
    fn draw_date_separator(ui: &mut egui::Ui, day: &str, theme: &Theme) {
        let label = Self::day_label(day);
        ui.add_space(10.0);
        let (row_rect, _resp) = ui
            .allocate_exact_size(egui::vec2(ui.available_width().max(0.0), 16.0), egui::Sense::hover());
        let font = egui::FontId::new(10.0, egui::FontFamily::Proportional);
        let galley = ui.ctx().fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), theme.text_dim));
        let half_gap = galley.rect.width() / 2.0 + 12.0;
        let line_y = row_rect.center().y;
        let stroke = egui::Stroke::new(1.0, theme.divider);
        ui.painter().hline(row_rect.left()..=(row_rect.center().x - half_gap), line_y, stroke);
        ui.painter().hline((row_rect.center().x + half_gap)..=row_rect.right(), line_y, stroke);
        ui.painter().text(row_rect.center(), egui::Align2::CENTER_CENTER, label, font, theme.text_dim);
        ui.add_space(6.0);
    }

    /// Human label for a `YYYY-MM-DD` day string.
    fn day_label(day: &str) -> String {
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let yesterday = (now - chrono::TimeDelta::days(1)).format("%Y-%m-%d").to_string();
        if day == today {
            "Today".to_string()
        } else if day == yesterday {
            "Yesterday".to_string()
        } else {
            chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .map(|d| d.format("%a, %b %e").to_string())
                .unwrap_or_else(|_| day.to_string())
        }
    }

    /// Rounded-square avatar with a letter label, allocated in row flow.
    fn draw_avatar(&self, ui: &mut egui::Ui, theme: &Theme, is_user: bool, size: f32) {
        let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(size, size));
        let color = if is_user { theme.primary } else { theme.accent };
        let label = if is_user { "U" } else { "AI" };
        let font_size = if is_user { size * 0.42 } else { size * 0.34 };
        ui.painter().rect(rect, 7.0, color, egui::Stroke::NONE, egui::StrokeKind::Middle);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::new(font_size, egui::FontFamily::Proportional),
            egui::Color32::WHITE,
        );
        ui.allocate_space(egui::vec2(size, size));
    }

    fn draw_scroll_to_bottom_button(&mut self, ui: &mut egui::Ui, theme: &Theme, button_opacity: f32) {
        let button_size = egui::vec2(36.0, 36.0);
        let button_pos = ui.max_rect().right_top() - egui::vec2(button_size.x + 16.0, 16.0);
        let button_rect = egui::Rect::from_min_size(button_pos, button_size);
        ui.scope_builder(egui::UiBuilder::new().max_rect(button_rect), |ui| {
            ui.set_max_size(button_size);
            ui.set_min_size(button_size);
            // Apply opacity via semi-transparent fill color (premultiplied alpha)
            let alpha = (button_opacity * 0.95 * 255.0) as u8;
            let fill_color = egui::Color32::from_rgba_premultiplied(
                theme.surface_light.r(),
                theme.surface_light.g(),
                theme.surface_light.b(),
                alpha
            );
            let border_color = egui::Color32::from_rgba_premultiplied(
                theme.border.r(),
                theme.border.g(),
                theme.border.b(),
                alpha
            );
            let scroll_btn = egui::Button::new(
                egui::RichText::new("↓").color(theme.text_primary).size(15.0),
            )
            .fill(fill_color)
            .stroke(egui::Stroke::new(1.0, border_color))
            .corner_radius(18);
            if ui.add(scroll_btn).clicked() {
                // Trigger auto-scroll on next frame
                if let Some(sid) = &self.selected_session_id {
                    if let Some(runtime) = self.session_store.get_mut(sid) {
                        runtime.chat_state.scroll_to_bottom_requested = true;
                    }
                }
            }
        });
    }

    /// Live streaming row shown while a response is in flight.
    /// Mirrors the committed message layout (avatar + AI bubble) so the text
    /// doesn't jump when the message is committed.
    fn draw_streaming_line(&mut self, ui: &mut egui::Ui, theme: &Theme, streaming: &(String, String)) {
        let (current_thinking, stream_buffer) = streaming;
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            self.draw_avatar(ui, theme, false, 28.0);
            ui.add_space(8.0);
            ui.scope(|ui| {
                ui.vertical(|ui| {
                    // Thinking: dim italic text in a quiet framed card.
                    if !current_thinking.is_empty() {
                        egui::Frame::NONE
                            .fill(theme.surface)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(10)
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(egui::RichText::new("Thinking…")
                                        .color(theme.text_dim)
                                        .italics()
                                        .size(11.0));
                                    ui.spinner();
                                });
                                ui.add_space(4.0);
                                ui.add(Self::breaking_label(
                                    current_thinking,
                                    egui::FontId::proportional(12.0),
                                    theme.text_dim,
                                    true,
                                ));
                            });
                        ui.add_space(8.0);
                    }
                    if !stream_buffer.is_empty() {
                        // Same bubble as a committed AI message.
                        egui::Frame::NONE
                            .fill(theme.ai_bg)
                            .stroke(egui::Stroke::new(1.0, theme.bubble_border))
                            .corner_radius(12)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.add(Self::breaking_label(
                                    stream_buffer,
                                    egui::FontId::proportional(13.5),
                                    theme.text_primary,
                                    false,
                                ));
                            });
                    } else if current_thinking.is_empty() {
                        // Nothing yet: pulsing typing dots in an empty bubble.
                        egui::Frame::NONE
                            .fill(theme.ai_bg)
                            .stroke(egui::Stroke::new(1.0, theme.bubble_border))
                            .corner_radius(12)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                let t = ui.ctx().input(|i| i.time) as f32;
                                let base = ui.cursor().min;
                                let dot = 5.0;
                                let gap = 4.0;
                                for k in 0..3 {
                                    let pulse = 0.5 + 0.5 * (t * 2.5 + k as f32 * 0.45).sin();
                                    let alpha = ((0.25 + 0.6 * pulse) * 255.0) as u8;
                                    let color = egui::Color32::from_rgba_premultiplied(
                                        theme.primary.r(),
                                        theme.primary.g(),
                                        theme.primary.b(),
                                        alpha,
                                    );
                                    ui.painter().circle_filled(
                                        base + egui::vec2(k as f32 * (dot + gap) + dot / 2.0, dot / 2.0),
                                        dot / 2.0,
                                        color,
                                    );
                                }
                                ui.allocate_space(egui::vec2(3.0 * dot + 2.0 * gap, dot));
                            });
                    }
                });
            });
        });
    }

    /// Live tool card shown while a tool call is executing.
    ///
    /// Shows the tool icon + name, its args preview (what it's doing), a live
    /// elapsed readout, and — for tools that stream (the shell) — a live tail
    /// of the output so long commands are visible while they run. The card
    /// carries the same indent as a committed tool card, so the transcript
    /// doesn't jump when the live card is replaced by the persisted result.
    fn draw_active_tool_card(&mut self, ui: &mut egui::Ui, tool: &crate::sessions::ActiveTool, theme: &Theme) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // Same indent as a committed tool card (28px avatar + 8px gap).
            ui.add_space(36.0);
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                // Pulsing border while running (accent, breathing alpha).
                let t = ui.ctx().input(|i| i.time) as f32;
                let pulse = 0.5 + 0.5 * (t * 2.2).sin();
                let alpha = ((0.35 + 0.5 * pulse) * 255.0) as u8;
                let border = egui::Color32::from_rgba_premultiplied(
                    theme.accent.r(),
                    theme.accent.g(),
                    theme.accent.b(),
                    alpha,
                );
                egui::Frame::NONE
                    .fill(theme.surface)
                    .stroke(egui::Stroke::new(1.0, border))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        // Header row: spinner + icon+name + args + elapsed.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.spinner();
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(format!("{} {}", Self::tool_icon(&tool.tool_name), tool.tool_name))
                                .color(theme.accent)
                                .strong()
                                .size(11.5));
                            if !tool.args_preview.is_empty() {
                                ui.add(Self::breaking_label(
                                    &tool.args_preview,
                                    egui::FontId::monospace(10.5),
                                    theme.text_dim,
                                    false,
                                ));
                            }
                            let elapsed = tool.started_at.elapsed();
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(egui::RichText::new(format!("running · {}", Self::format_duration(elapsed.as_millis() as u64)))
                                    .color(theme.text_dim)
                                    .size(9.5));
                            });
                        });
                        // Live output tail (shell). Latest lines only.
                        if !tool.live_output.is_empty() {
                            ui.add_space(4.0);
                            Self::code_block(ui, theme, |ui| {
                                ui.vertical(|ui| {
                                    let lines: Vec<&str> = tool.live_output.lines().collect();
                                    // Show at most the last 6 lines (latest tail).
                                    let start = lines.len().saturating_sub(6);
                                    for line in &lines[start..] {
                                        ui.add(Self::breaking_label(
                                            line,
                                            egui::FontId::monospace(11.0),
                                            theme.code_text,
                                            false,
                                        ));
                                    }
                                });
                            });
                        }
                    });
            });
        });
    }

    /// Small per-tool glyph for the tool card headers.
    fn tool_icon(name: &str) -> &'static str {
        match name {
            "shell" => "⚡",
            "read_file" => "📄",
            "write_file" | "append_file" | "apply_diff" => "✏️",
            "list_dir" | "mkdir" => "📁",
            "search_files" | "search_content" => "🔍",
            "copy" | "move" | "delete" | "file_info" => "📦",
            "web_search" => "🌐",
            "fetch_url" => "🔗",
            "calculation" => "🧮",
            "time" => "🕐",
            "save_memory" | "update_memory" | "search_memory"
            | "consolidate_memories" | "delete_memory" => "🧠",
            "handoff" => "🔀",
            "restart" => "🔁",
            _ => "🔧",
        }
    }

    /// Compact duration for chips: `42ms`, `1.3s`, `2m 05s`.
    fn format_duration(ms: u64) -> String {
        if ms < 1000 {
            format!("{}ms", ms)
        } else if ms < 60_000 {
            format!("{:.1}s", ms as f64 / 1000.0)
        } else {
            let m = ms / 60_000;
            let s = (ms % 60_000) / 1000;
            format!("{}m {:02}s", m, s)
        }
    }

    /// Check if the scroll area is at the bottom using ScrollAreaOutput after render.
    fn is_at_bottom_from_output(&self, output: &egui::containers::scroll_area::ScrollAreaOutput<()>) -> bool {
        let content_height = output.content_size.y;
        let viewport_height = output.inner_rect.height();
        
        // If content fits in viewport, no scrolling needed - at bottom
        if content_height <= viewport_height {
            return true;
        }
        
        let max_offset = content_height - viewport_height;
        let current_offset = output.state.offset.y;
        current_offset >= max_offset - Self::SCROLL_BOTTOM_THRESHOLD
    }

    /// Wrapping label that also splits long unbreakable words (URLs, long
    /// paths, tokens). egui's default word-wrap prefers word boundaries and
    /// will overflow the bubble when a single word is wider than the line,
    /// so we enable `break_anywhere` on the layout job.
    fn breaking_label(
        text: impl AsRef<str>,
        font_id: egui::FontId,
        color: egui::Color32,
        italics: bool,
    ) -> egui::Label {
        let mut job = egui::epaint::text::LayoutJob::default();
        let mut tf = egui::epaint::text::TextFormat::default();
        tf.font_id = font_id;
        tf.color = color;
        tf.italics = italics;
        job.append(text.as_ref(), 0.0, tf);
        job.wrap.break_anywhere = true;
        egui::Label::new(job).wrap()
    }


    /// Strip <think</think>... tags wrapper from thinking content for display.
    fn strip_thinking_tags(content: &str) -> String {
        let t = content.trim();
        let t = t.strip_prefix("<think>").unwrap_or(t);
        t.strip_suffix("</think>").unwrap_or(t).trim().to_string()
    }

    pub(super) fn draw_message(
        &mut self,
        ui: &mut egui::Ui,
        message: &ChatMessage,
        index: usize,
        theme: &Theme,
    ) {
        let is_user = message.role == "user";
        let is_editing = self.selected_session_id.as_ref().map(|sid| {
            self.session_store.get(sid).map(|r| r.chat_state.editing_message_index == Some(index)).unwrap_or(false)
        }).unwrap_or(false);

        // Tool messages render as a compact collapsible card (result hidden
        // by default, expandable on click) instead of a full bubble.
        if message.kind == MessageKind::Tool && !is_editing {
            self.draw_tool_card(ui, message, index, theme);
            return;
        }

        let bubble_bg = if is_user {
            theme.user_bg
        } else if message.kind == MessageKind::Tool {
            theme.tool_bg
        } else {
            theme.ai_bg
        };
        let text_color = if is_user { egui::Color32::WHITE } else { theme.text_primary };

        // Gap between messages.
        ui.add_space(14.0);

        // Row flows right-to-left for user messages so the whole
        // avatar + bubble group sits at the right edge.
        let row_layout = if is_user {
            egui::Layout::right_to_left(egui::Align::TOP)
        } else {
            egui::Layout::left_to_right(egui::Align::TOP)
        };

        ui.with_layout(row_layout, |ui| {
            self.draw_avatar(ui, theme, is_user, 28.0);
            ui.add_space(8.0); // gap between avatar and bubble

            // Content column (bubble + hover metadata)
            ui.scope(|ui| {
                // Bubbles span the full chat width, docked edge-to-edge like
                // the input field (user bubbles still sit at the right edge
                // because the row flows right-to-left).
                // Handle right-click context menu for edit/delete
                let response = ui.interact(
                    ui.max_rect(),
                    ui.id().with("msg_ctx").with(index),
                    egui::Sense::click(),
                );
                if !is_editing && response.secondary_clicked() {
                    response.context_menu(|menu_ui| {
                        menu_ui.set_min_width(120.0);
                        if menu_ui.button("Edit").clicked() {
                            if let Some(sid) = &self.selected_session_id {
                                if let Some(runtime) = self.session_store.get_mut(sid) {
                                    runtime.chat_state.editing_message_index = Some(index);
                                    runtime.chat_state.editing_message_content = message.content.clone();
                                }
                            }
                        }
                        if menu_ui.button("Delete").clicked() {
                            self.delete_message(index);
                        }
                    });
                }
                
                ui.vertical(|ui| {
                    let inner = egui::Frame::NONE
                        .fill(bubble_bg)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(12)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            if layout_dbg_enabled() {
                                eprintln!("[ldbg] msg {:>3} frame avail_w={:.1}", index, ui.available_width());
                            }
                            if is_editing {
                                if let Some(sid) = &self.selected_session_id {
                                    if let Some(runtime) = self.session_store.get_mut(sid) {
                                        ui.add_sized(
                                            egui::vec2(ui.available_width().max(160.0), 80.0),
                                            egui::TextEdit::multiline(&mut runtime.chat_state.editing_message_content),
                                        );
                                    }
                                }
                            } else {
                                // Display image if present
                                if let Some(ref img_data) = message.image {
                                    if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, img_data) {
                                        // Per-message id so multiple images don't
                                        // share one texture slot.
                                        let img = egui::Image::from_bytes(format!("chat_image_{}", index), decoded);
                                        let max_img_width = (ui.available_width() - 10.0).max(50.0);
                                        ui.add(img.max_size(egui::Vec2::new(max_img_width, 300.0)));
                                    }
                                }
                                // Branch on message kind — no string-prefix sniffing.
                                // (Tool messages never reach the bubble: they are
                                // rendered as collapsible cards above.)
                                if message.kind == MessageKind::Thinking {
                                    // Thinking message — render dim and italic
                                    ui.add(Self::breaking_label(
                                        &message.content,
                                        egui::FontId::proportional(12.5),
                                        theme.text_dim,
                                        true,
                                    ));
                                } else {
                                    // Normal message — strip any legacy <think> tags
                                    let display_content = if message.content.contains("<think>") || message.content.contains("</think>") {
                                        Self::strip_thinking_tags(&message.content)
                                    } else {
                                        message.content.clone()
                                    };
                                    ui.add(Self::breaking_label(
                                        display_content,
                                        egui::FontId::proportional(13.5),
                                        text_color,
                                        false,
                                    ));
                                }
                            }
                        });

                        if layout_dbg_enabled() {
                            eprintln!(
                                "[ldbg] msg {:>3} bubble_w={:.1} bubble_rect={:?}",
                                index,
                                inner.response.rect.width(),
                                inner.response.rect
                            );
                        }

                        // Timestamp — always visible, part of the layout flow.
                        let ts = crate::types::timestamp_time(&message.timestamp);
                        if !ts.is_empty() {
                            ui.add(egui::Label::new(
                                egui::RichText::new(ts).color(theme.text_dim).size(9.5),
                            ));
                        }

                        // S2: feedback (👍/👎) under assistant answers only.
                        if !is_user && !is_editing && message.kind == MessageKind::Normal {
                            self.draw_feedback_row(ui, index, theme);
                        }

                        // Hover reveal: copy button in the top-right corner.
                        // Placed (not laid out) so it never shifts the message flow.
                        //
                        // Gate visibility on the raw pointer position, NOT on
                        // `inner.response.hovered()`: the button is placed on top
                        // of the bubble, and while the pointer is over it, the
                        // bubble frame (a hover-only widget) stops reporting
                        // `hovered` because the click-sensitive button covers it.
                        // Gating on the frame's hover made the button vanish the
                        // instant the pointer touched it — a per-frame show/hide
                        // flicker — and egui drops the pending click when the
                        // widget disappears, so the click never registered and
                        // nothing was copied. The button rect lies inside the
                        // frame rect, so "pointer over the bubble" covers both.
                        if !is_editing
                            && ui
                                .ctx()
                                .pointer_hover_pos()
                                .is_some_and(|pos| inner.response.rect.contains(pos))
                        {
                            let btn_size = egui::vec2(16.0, 16.0);
                            let btn_rect = egui::Rect::from_min_size(
                                inner.response.rect.right_top() - egui::vec2(btn_size.x + 3.0, 3.0),
                                btn_size,
                            );
                            let copy_resp = ui.put(
                                btn_rect,
                                egui::Button::new(
                                    egui::RichText::new("⧉").color(theme.text_dim).size(11.0),
                                )
                                .fill(theme.hover_bg)
                                .corner_radius(4),
                            );
                            if copy_resp.clicked() {
                                ui.ctx().copy_text(message.content.clone());
                            }
                        }
                });
            });
        });

        // Handle keyboard shortcuts when editing
        if is_editing {
            ui.ctx().input(|i| {
                if i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl {
                    self.commit_message_edit(index);
                }
                if i.key_pressed(egui::Key::Escape) {
                    if let Some(sid) = &self.selected_session_id {
                        if let Some(runtime) = self.session_store.get_mut(sid) {
                            runtime.chat_state.editing_message_index = None;
                            runtime.chat_state.editing_message_content.clear();
                        }
                    }
                }
            });
        }
    }

    pub(super) fn commit_message_edit(&mut self, index: usize) {
        let new_content = match &self.selected_session_id {
            Some(sid) => {
                self.session_store.get(sid).map(|r| r.chat_state.editing_message_content.clone())
            }
            None => return,
        };
        let new_content = match new_content {
            Some(c) => c,
            None => return,
        };
        
        // Update chat_display
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                if index < runtime.chat_state.messages.len() {
                    runtime.chat_state.messages[index].content = new_content.clone();
                }
            }
        }
        
        // Update the underlying client conversation, then persist via the
        // single save path.
        if let Some(sid) = self.selected_session_id.clone() {
            if let Some(runtime) = self.session_store.get(&sid) {
                // The display and the store are SEPARATE arrays that drift apart as
                // soon as the display gains entries the store does not hold (e.g.
                // Thinking blocks: one display entry per round, while the store folds
                // reasoning into the assistant message). A display index is only a
                // valid store index while the two are the same length; otherwise
                // `conv[index]` is a different (later) message and the edit would
                // silently rewrite the wrong entry. When misaligned, the edit applies
                // to the display only (it is a view; the store stays authoritative).
                let aligned = runtime.chat_state.messages.len()
                    == runtime.client.conversation().lock().unwrap().len();
                if aligned {
                    let mut conv = runtime.client.conversation().lock().unwrap();
                    if index < conv.len() {
                        conv[index].content = new_content;
                    }
                }
            }
            if let Err(e) = self.save_session_for(&sid) {
                eprintln!("Failed to save session after edit: {}", e);
            }
        }
        
        // Clear edit state
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                runtime.chat_state.editing_message_index = None;
                runtime.chat_state.editing_message_content.clear();
            }
        }
        // In-place edit keeps the message count unchanged, so force the display
        // snapshot to rebuild next frame (the len-based check would miss it).
        self.display_dirty = true;
    }

    /// Compact collapsible tool card.
    ///
    /// Collapsed row: status icon, tool icon + name, the call's args preview
    /// (what it did), a result/error summary, a duration chip, and the time.
    /// Clicking the header row expands the full result detail (right-click
    /// opens the delete menu). The card is indented to sit under the AI
    /// message column and has no avatar, keeping tool chatter visually quiet
    /// compared to normal messages.
    fn draw_tool_card(&mut self, ui: &mut egui::Ui, message: &ChatMessage, index: usize, theme: &Theme) {
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
                let ts = crate::types::timestamp_time(&message.timestamp);

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

    /// Render a tool result that is valid JSON with smart field extraction.
    /// Called only with the tool card already expanded, so long content
    /// (e.g. file reads) is shown directly in a height-capped scroll area.
    fn draw_tool_json_result(&self, ui: &mut egui::Ui, json: &serde_json::Value, raw: &str, theme: &Theme) {
        // Shell output: {exit_code, stdout, stderr, duration_ms, truncated} —
        // render a status line plus stdout/stderr as line-by-line code blocks
        // (one giant wrapped label is unreadable for command output).
        if json.get("exit_code").is_some() {
            let exit_code = json.get("exit_code").and_then(|v| v.as_u64()).unwrap_or(0);
            let dur = json.get("duration_ms").and_then(|v| v.as_u64());
            let truncated = json.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false);
            ui.horizontal(|ui| {
                if exit_code == 0 {
                    ui.colored_label(theme.success, "✓");
                } else {
                    ui.colored_label(theme.warning, "✗");
                }
                ui.label(egui::RichText::new(format!("exit code {}", exit_code))
                    .color(if exit_code == 0 { theme.text_dim } else { theme.warning })
                    .size(11.0));
                if let Some(d) = dur {
                    ui.label(egui::RichText::new(format!("· {}", Self::format_duration(d)))
                        .color(theme.text_dim)
                        .size(10.5));
                }
                if truncated {
                    ui.label(egui::RichText::new("· output truncated")
                        .color(theme.text_dim)
                        .size(10.5));
                }
            });
            let stream_block = |ui: &mut egui::Ui, title: &str, value: &str, color: egui::Color32| {
                if value.trim().is_empty() {
                    return;
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new(title)
                    .color(theme.text_dim)
                    .size(10.0)
                    .monospace());
                ui.add_space(2.0);
                Self::code_block(ui, theme, |ui| {
                    let lines: Vec<&str> = value.lines().collect();
                    const MAX_LINES: usize = 300;
                    ui.vertical(|ui| {
                        for line in &lines[..lines.len().min(MAX_LINES)] {
                            ui.add(Self::breaking_label(
                                line,
                                egui::FontId::monospace(11.5),
                                color,
                                false,
                            ));
                        }
                        if lines.len() > MAX_LINES {
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(format!(
                                "… ({} more lines)",
                                lines.len() - MAX_LINES
                            ))
                            .color(theme.text_dim)
                            .size(10.5));
                        }
                    });
                });
            };
            stream_block(
                ui,
                "stdout",
                json.get("stdout").and_then(|v| v.as_str()).unwrap_or(""),
                theme.code_text,
            );
            stream_block(
                ui,
                "stderr",
                json.get("stderr").and_then(|v| v.as_str()).unwrap_or(""),
                theme.warning,
            );
            return;
        }
        // Check for common structured patterns
        if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
            // Has a path field — likely a file operation result
            let is_file_read = json.get("content").is_some();
            let is_file_write = json.get("bytes_written").is_some() || json.get("success").is_some();
            let is_dir_list = json.get("entries").is_some();

            if is_dir_list {
                // Directory listing: show path badge + entries list
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(entries) = json.get("entries").and_then(|v| v.as_array()) {
                    let max_entries = 50;
                    let display_entries: Vec<&serde_json::Value> = entries.iter().take(max_entries).collect();
                    Self::code_block(ui, theme, |ui| {
                            for entry in display_entries {
                                // New list_dir returns {name, type, size} objects;
                                // older sessions stored plain strings.
                                let line = if let Some(s) = entry.as_str() {
                                    s.to_string()
                                } else {
                                    let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let typ = entry.get("type").and_then(|v| v.as_str()).unwrap_or("file");
                                    let size = entry.get("size").and_then(|v| v.as_u64())
                                        .map(|n| format!("  {} bytes", n))
                                        .unwrap_or_default();
                                    format!("{}  [{}{}]", name, typ, size)
                                };
                                ui.add(Self::breaking_label(
                                    line,
                                    egui::FontId::monospace(13.0),
                                    theme.code_text,
                                    false,
                                ));
                            }
                            if entries.len() > max_entries {
                                ui.label(egui::RichText::new(format!("... and {} more entries", entries.len() - max_entries))
                                    .color(theme.text_dim)
                                    .size(10.0));
                            }
                        });
                }
            } else if is_file_read {
                // File read: path badge + content in a height-capped scroll area
                // (the surrounding tool card already controls expand/collapse).
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                    Self::code_block(ui, theme, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                ui.add(Self::breaking_label(
                                    content,
                                    egui::FontId::monospace(12.5),
                                    theme.code_text,
                                    false,
                                ));
                            });
                    });
                }
            } else if is_file_write {
                // File write: show compact success badge
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(bytes) = json.get("bytes_written").and_then(|v| v.as_u64()) {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme.success, format!("✓ Written {} byte{}", bytes, if bytes == 1 { "" } else { "s" }));
                    });
                } else if let Some(success) = json.get("success").and_then(|v| v.as_bool()) {
                    if success {
                        ui.colored_label(theme.success, "✓ File written successfully");
                    }
                }
            } else {
                // Generic JSON with path — render as structured key-value
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                self.draw_tool_json_kv(ui, json, theme);
            }
        } else {
            // No path field — check for other common patterns
            if let Some(expr) = json.get("expression").and_then(|v| v.as_str()) {
                if let Some(result) = json.get("result").and_then(|v| v.as_f64()) {
                    // Calculation result
                    // The expression wraps at the block width; the result goes
                    // on its own line so a long expression cannot push it past
                    // the right edge.
                    ui.add(Self::breaking_label(
                        expr,
                        egui::FontId::monospace(13.0),
                        theme.text_secondary,
                        false,
                    ));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("= ").color(theme.text_dim));
                        ui.colored_label(theme.success, format!("{}", result));
                    });
                } else {
                    self.draw_tool_json_kv(ui, json, theme);
                }
            } else if json.is_object() {
                // Generic JSON object — render as key-value pairs
                self.draw_tool_json_kv(ui, json, theme);
            } else {
                // Array or scalar — fall back to plain rendering
                self.draw_tool_plain_result(ui, raw, theme);
            }
        }
    }

    /// Render JSON as a key-value list.
    fn draw_tool_json_kv(&self, ui: &mut egui::Ui, json: &serde_json::Value, theme: &Theme) {
        Self::code_block(ui, theme, |ui| {
                match json {
                    serde_json::Value::Object(map) => {
                        for (key, value) in map {
                            ui.horizontal(|ui| {
                                ui.add(Self::breaking_label(
                                    format!("{}:", key),
                                    egui::FontId::monospace(11.0),
                                    theme.text_secondary,
                                    false,
                                ));
                                let val_str = Self::json_value_to_string(value);
                                ui.add(Self::breaking_label(
                                    val_str,
                                    egui::FontId::monospace(11.0),
                                    theme.code_text,
                                    false,
                                ));
                            });
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            ui.add(Self::breaking_label(
                                Self::json_value_to_string(item),
                                egui::FontId::monospace(11.0),
                                theme.code_text,
                                false,
                            ));
                        }
                    }
                    other => {
                        ui.add(Self::breaking_label(
                            Self::json_value_to_string(other),
                            egui::FontId::monospace(11.0),
                            theme.code_text,
                            false,
                        ));
                    }
                }
            });
    }

    /// Convert a JSON value to a display string.
    fn json_value_to_string(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Array(arr) => {
                let items: Vec<String> = arr.iter().map(Self::json_value_to_string).collect();
                format!("[{}]", items.join(", "))
            }
            serde_json::Value::Object(map) => {
                let pairs: Vec<String> = map.iter()
                    .map(|(k, v)| format!("{}: {}", k, Self::json_value_to_string(v)))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
        }
    }

    /// Draw a clickable file path chip.
    fn draw_tool_path_badge(&self, ui: &mut egui::Ui, path: &str, theme: &Theme) {
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

    /// Pixel width of a single glyph in the given font.
    /// egui memoizes layout results, so this stays cheap across frames.
    fn char_width(ui: &egui::Ui, font_id: &egui::FontId) -> f32 {
        const SAMPLE: &str = "0123456789";
        let width = ui.ctx().fonts_mut(|fonts| {
            fonts.layout_no_wrap(SAMPLE.to_string(), font_id.clone(), egui::Color32::WHITE)
                .rect
                .width()
        });
        (width / SAMPLE.len() as f32).max(1.0)
    }


    /// S2: 👍/👎 feedback row under an assistant answer.
    ///
    /// 👍 saves immediately; 👎 opens a one-line optional comment with
    /// save (✓) / cancel (✕). A rated message shows the chosen button
    /// highlighted with both buttons disabled (no double-save).
    fn draw_feedback_row(&mut self, ui: &mut egui::Ui, index: usize, theme: &Theme) {
        let Some(sid) = self.selected_session_id.clone() else {
            return;
        };
        let (rated, comment_open, mut comment) = match self.session_store.get(&sid) {
            Some(rt) => (
                rt.chat_state.message_ratings.get(&index).cloned(),
                rt.chat_state.feedback_comment_for == Some(index),
                rt.chat_state.feedback_comment.clone(),
            ),
            None => return,
        };

        ui.add_space(2.0);
        let mut save_good = false;
        let mut open_bad = false;
        let mut save_bad = false;
        let mut cancel = false;

        ui.horizontal(|ui| {
            ui.set_height(16.0);
            let is_good = rated.as_deref() == Some("good");
            let is_bad = rated.as_deref() == Some("bad");

            if ui
                .add_enabled(
                    rated.is_none(),
                    egui::Button::new(egui::RichText::new("👍").size(if is_good { 13.0 } else { 11.0 }))
                        .fill(if is_good { theme.primary } else { theme.hover_bg })
                        .corner_radius(4),
                )
                .clicked()
            {
                save_good = true;
            }
            if ui
                .add_enabled(
                    rated.is_none(),
                    egui::Button::new(egui::RichText::new("👎").size(if is_bad { 13.0 } else { 11.0 }))
                        .fill(if is_bad { theme.primary } else { theme.hover_bg })
                        .corner_radius(4),
                )
                .clicked()
            {
                open_bad = true;
            }
            if rated.is_some() {
                ui.label(egui::RichText::new("rated").color(theme.text_dim).size(9.5));
            }
            if comment_open {
                ui.add(
                    egui::TextEdit::singleline(&mut comment)
                        .desired_width(220.0)
                        .hint_text("Optional comment…"),
                );
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("✓").color(theme.text_dim).size(11.0))
                            .fill(theme.success)
                            .corner_radius(4),
                    )
                    .clicked()
                {
                    save_bad = true;
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("✕").color(theme.text_dim).size(11.0))
                            .fill(theme.hover_bg)
                            .corner_radius(4),
                    )
                    .clicked()
                {
                    cancel = true;
                }
            }
        });

        // Persist the typed comment back to the session state (the field edits
        // a per-frame copy).
        if cancel {
            if let Some(rt) = self.session_store.get_mut(&sid) {
                rt.chat_state.feedback_comment_for = None;
                rt.chat_state.feedback_comment.clear();
            }
        }
        if save_good {
            self.save_message_feedback(index, true, "");
        } else if save_bad {
            self.save_message_feedback(index, false, &comment);
        } else if open_bad {
            if let Some(rt) = self.session_store.get_mut(&sid) {
                rt.chat_state.feedback_comment_for = Some(index);
                rt.chat_state.feedback_comment.clear();
            }
        } else if comment_open {
            // Field still open after this frame (no save/cancel): persist the
            // typed comment. On a failed save the field stays open for retry.
            if let Some(rt) = self.session_store.get_mut(&sid) {
                if rt.chat_state.feedback_comment_for == Some(index) {
                    rt.chat_state.feedback_comment = comment;
                }
            }
        }
    }

    /// Render a plain (non-JSON) tool result as a monospace code block.
    fn draw_tool_plain_result(&self, ui: &mut egui::Ui, text: &str, theme: &Theme) {
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
    fn code_block(ui: &mut egui::Ui, theme: &Theme, add_contents: impl FnOnce(&mut egui::Ui)) {
        egui::Frame::NONE
            .fill(theme.code_bg)
            .stroke(egui::Stroke::new(1.0, theme.code_border))
            .corner_radius(6)
            .inner_margin(egui::Margin::same(8))
            .show(ui, add_contents);
    }

    pub(super) fn delete_message(&mut self, index: usize) {
        if let Some(sid) = self.selected_session_id.clone() {
            if let Some(runtime) = self.session_store.get_mut(&sid) {
                if index < runtime.chat_state.messages.len() {
                    runtime.chat_state.messages.remove(index);
                    // S2: keep the feedback state index-aligned after deletion
                    // (keys below stay, the deleted one drops, higher ones shift).
                    runtime.chat_state.message_ratings = std::mem::take(&mut runtime.chat_state.message_ratings)
                        .into_iter()
                        .map(|(k, v)| (if k > index { k - 1 } else { k }, v))
                        .collect();
                    runtime.chat_state.feedback_comment_for =
                        match runtime.chat_state.feedback_comment_for {
                            Some(f) if f == index => None,
                            Some(f) if f > index => Some(f - 1),
                            other => other,
                        };
                    // Also remove from the underlying client conversation — but only
                    // while the display and the store are the same length. They are
                    // separate arrays that drift apart as soon as the display gains
                    // entries the store does not hold (e.g. Thinking blocks: one
                    // display entry per round, while the store folds reasoning into
                    // the assistant message); once misaligned, a display index points
                    // at a DIFFERENT (later) store message, so removing conv[index]
                    // would silently delete the wrong message. When misaligned the
                    // delete applies to the display only (it is a view; the store
                    // stays authoritative and the message returns on reload).
                    let aligned = runtime.chat_state.messages.len() + 1
                        == runtime.client.conversation().lock().unwrap().len();
                    if aligned {
                        let mut conv = runtime.client.conversation().lock().unwrap();
                        if index < conv.len() {
                            conv.remove(index);
                        }
                    }
                }
            }
            if let Err(e) = self.save_session_for(&sid) {
                eprintln!("Failed to save session after delete: {}", e);
            }
        }
    }
}
