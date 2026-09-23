use eframe::egui;

use super::state::ChatApp;
use super::theme::Theme;

// ── TEMPORARY layout debugging (set WUFF_LAYOUT_DBG=1 to enable) ────────
static LAYOUT_DBG_FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn layout_dbg_enabled() -> bool {
    std::env::var_os("WUFF_LAYOUT_DBG").is_some()
        && (2..7).contains(&(LAYOUT_DBG_FRAME.load(std::sync::atomic::Ordering::Relaxed) % 1000))
}
// ────────────────────────────────────────────────────────────────────────

mod bubbles;
mod tool_cards;

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
        let active_tools: Vec<wuffagent_core::sessions::ActiveTool> = self
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
                                if let Some(day) = wuffagent_core::types::timestamp_day(&msg.timestamp) {
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

}
