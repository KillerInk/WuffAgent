use eframe::egui;

use super::super::state::ChatApp;
use super::ChatArea;
use super::super::state::groups::SessionState;
use super::super::theme::Theme;
use wuffagent_core::types::{ChatMessage, MessageKind};

// ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ TEMPORARY layout debugging (set WUFF_LAYOUT_DBG=1 to enable) ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬
static LAYOUT_DBG_FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn layout_dbg_enabled() -> bool {
    std::env::var_os("WUFF_LAYOUT_DBG").is_some()
        && (2..7).contains(&(LAYOUT_DBG_FRAME.load(std::sync::atomic::Ordering::Relaxed) % 1000))
}
// ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬ÃƒÂ¢Ã¢â‚¬ÂÃ¢â€šÂ¬

/// Strip <think...> tags wrapper from thinking content for display.
fn strip_thinking_tags(content: &str) -> String {
    let t = content.trim();
    let t = t.strip_prefix("<think>").unwrap_or(t);
    t.strip_suffix("</think>").unwrap_or(t).trim().to_string()
}

/// Per-frame display content for a message: borrows the content when no
/// legacy think tags are present (the common case), so the hot draw path
/// does not clone the message body every frame.
fn display_content_ref<'a>(content: &'a str) -> std::borrow::Cow<'a, str> {
    if content.contains("<think>") || content.contains("</think>") {
        std::borrow::Cow::Owned(strip_thinking_tags(content))
    } else {
        std::borrow::Cow::Borrowed(content)
    }
}

/// Live streaming row shown while a response is in flight.
/// Mirrors the committed message layout (avatar + AI bubble) so the text
/// doesn't jump when the message is committed.
pub(super) fn draw_streaming_line(
    ui: &mut egui::Ui,
    theme: &Theme,
    streaming: &(String, String),
) {
        let (current_thinking, stream_buffer) = streaming;
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            super::draw_avatar(ui, theme, false, 28.0);
            ui.add_space(8.0);
            ui.scope(|ui| {
                // Dock to the full remaining row width (mirrors draw_message).
                ui.take_available_width();
                ui.vertical(|ui| {
                    // Thinking: dim italic text in a quiet framed card.
                    if !current_thinking.is_empty() {
                        egui::Frame::NONE
                            .fill(theme.surface)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(10)
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.take_available_width();
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(
                                        egui::RichText::new("ThinkingÃƒÂ¢Ã¢â€šÂ¬Ã‚Â¦")
                                            .color(theme.text_dim)
                                            .italics()
                                            .size(11.0),
                                    );
                                    ui.spinner();
                                });
                                ui.add_space(4.0);
                                // Dim markdown while the thinking streams in.
                                // Streaming variant: the buffer changes every
                                // token, so it must not pollute the parse cache.
                                super::markdown::draw_markdown_streaming(
                                    ui,
                                    current_thinking,
                                    12.0,
                                    theme.text_dim,
                                    theme,
                                    true,
                                );
                            });
                        ui.add_space(8.0);
                    }
                    if !stream_buffer.is_empty() {
                        // Same bubble as a committed AI message ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â rendered as
                        // markdown live, so bold/code/lists/tables appear while
                        // the answer streams (matching the committed view).
                        egui::Frame::NONE
                            .fill(theme.ai_bg)
                            .stroke(egui::Stroke::new(1.0, theme.bubble_border))
                            .corner_radius(12)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.take_available_width();
                                // Streaming variant: the buffer changes every
                                // token, so it must not pollute the parse cache.
                                super::markdown::draw_markdown_streaming(
                                    ui,
                                    stream_buffer,
                                    13.5,
                                    theme.text_primary,
                                    theme,
                                    false,
                                );
                            });
                    } else if current_thinking.is_empty() {
                        // Nothing yet: pulsing typing dots in an empty bubble.
                        egui::Frame::NONE
                            .fill(theme.ai_bg)
                            .stroke(egui::Stroke::new(1.0, theme.bubble_border))
                            .corner_radius(12)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.take_available_width();
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
                                        base + egui::vec2(
                                            k as f32 * (dot + gap) + dot / 2.0,
                                            dot / 2.0,
                                        ),
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
    /// elapsed readout, and ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â for tools that stream (the shell) ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â a live tail
    /// of the output so long commands are visible while they run. The card
    /// carries the same indent as a committed tool card, so the transcript
    /// doesn't jump when the live card is replaced by the persisted result.
pub(super) fn draw_active_tool_card(
    ui: &mut egui::Ui,
    tool: &wuffagent_core::sessions::ActiveTool,
    theme: &Theme,
) {
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
                        ui.take_available_width();
                        // Header row: spinner + icon+name + args + elapsed.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.spinner();
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {}",
                                    super::tool_icon(&tool.tool_name),
                                    tool.tool_name
                                ))
                                .color(theme.accent)
                                .strong()
                                .size(11.5),
                            );
                            if !tool.args_preview.is_empty() {
                                ui.add(super::breaking_label(
                                    &tool.args_preview,
                                    egui::FontId::monospace(10.5),
                                    theme.text_dim,
                                    false,
                                ));
                            }
                            let elapsed = tool.started_at.elapsed();
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "running Ãƒâ€šÃ‚Â· {}",
                                            super::format_duration(elapsed.as_millis() as u64)
                                        ))
                                        .color(theme.text_dim)
                                        .size(9.5),
                                    );
                                },
                            );
                        });
                        // Live output tail (shell). Latest lines only.
                        if !tool.live_output.is_empty() {
                            ui.add_space(4.0);
                            super::code_block(ui, theme, |ui| {
                                ui.vertical(|ui| {
                                    let lines: Vec<&str> = tool.live_output.lines().collect();
                                    // Show at most the last 6 lines (latest tail).
                                    let start = lines.len().saturating_sub(6);
                                    for line in &lines[start..] {
                                        ui.add(super::breaking_label(
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

impl ChatApp {
    pub(super) fn draw_message(
        &mut self,
        ui: &mut egui::Ui,
        message: &ChatMessage,
        index: usize,
        theme: &Theme,
    ) {
        let is_user = message.role == "user";
        let is_editing = self
            .sessions
            .selected_session_id
            .as_ref()
            .map(|sid| {
                self.sessions
                    .session_store
                    .get(sid)
                    .map(|r| r.chat_state.editing_message_index == Some(index))
                    .unwrap_or(false)
            })
            .unwrap_or(false);

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
        let text_color = if is_user {
            egui::Color32::WHITE
        } else {
            theme.text_primary
        };

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
            super::draw_avatar(ui, theme, is_user, 28.0);
            ui.add_space(8.0); // gap between avatar and bubble

            // Content column (bubble + hover metadata)
            ui.scope(|ui| {
                // Bubbles span the full chat width, docked edge-to-edge like
                // the input field (user bubbles still sit at the right edge
                // because the row flows right-to-left).
                ui.take_available_width();
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
                            if let Some(sid) = &self.sessions.selected_session_id {
                                if let Some(runtime) = self.sessions.session_store.get_mut(sid) {
                                    runtime.chat_state.editing_message_index = Some(index);
                                    runtime.chat_state.editing_message_content =
                                        message.content.clone();
                                }
                            }
                        }
                        if menu_ui.button("Delete").clicked() {
                            delete_message(&mut self.sessions, index);
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
                            ui.take_available_width();
                            if layout_dbg_enabled() {
                                eprintln!(
                                    "[ldbg] msg {:>3} frame avail_w={:.1}",
                                    index,
                                    ui.available_width()
                                );
                            }
                            if is_editing {
                                if let Some(sid) = &self.sessions.selected_session_id {
                                    if let Some(runtime) = self.sessions.session_store.get_mut(sid)
                                    {
                                        ui.add_sized(
                                            egui::vec2(ui.available_width().max(160.0), 80.0),
                                            egui::TextEdit::multiline(
                                                &mut runtime.chat_state.editing_message_content,
                                            ),
                                        );
                                    }
                                }
                            } else {
                                // Display image if present
                                if let Some(ref img_data) = message.image {
                                    if let Ok(decoded) = base64::Engine::decode(
                                        &base64::engine::general_purpose::STANDARD,
                                        img_data,
                                    ) {
                                        // Per-message id so multiple images don't
                                        // share one texture slot.
                                        let img = egui::Image::from_bytes(
                                            format!("chat_image_{}", index),
                                            decoded,
                                        );
                                        let max_img_width = (ui.available_width() - 10.0).max(50.0);
                                        ui.add(img.max_size(egui::Vec2::new(max_img_width, 300.0)));
                                    }
                                }
                                // Branch on message kind ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â no string-prefix sniffing.
                                // (Tool messages never reach the bubble: they are
                                // rendered as collapsible cards above.)
                                if message.kind == MessageKind::Thinking {
                                    // Thinking message ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â dim markdown (regular
                                    // weight, dimmed links/code).
                                    super::markdown::draw_markdown_dimmed(
                                        ui,
                                        &message.content,
                                        12.5,
                                        theme.text_dim,
                                        theme,
                                        true,
                                    );
                                } else {
                                    // Normal message ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â strip legacy think tags.
                                    // Borrows the content when no tags are present
                                    // (the common case), avoiding a per-frame clone.
                                    let display_content =
                                        display_content_ref(&message.content);
                                    if is_user {
                                        // User messages stay plain text.
                                        ui.add(super::breaking_label(
                                            display_content,
                                            egui::FontId::proportional(13.5),
                                            text_color,
                                            false,
                                        ));
                                    } else {
                                        // AI messages render as markdown
                                        // (bold, code blocks, lists, tablesÃƒÂ¢Ã¢â€šÂ¬Ã‚Â¦).
                                        super::markdown::draw_markdown(
                                            ui,
                                            &display_content,
                                            13.5,
                                            text_color,
                                            theme,
                                        );
                                    }
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

                    // Timestamp ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â always visible, part of the layout flow.
                    let ts = wuffagent_core::types::timestamp_time(&message.timestamp);
                    if !ts.is_empty() {
                        ui.add(egui::Label::new(
                            egui::RichText::new(ts).color(theme.text_dim).size(9.5),
                        ));
                    }

                    // S2: feedback (ÃƒÂ°Ã…Â¸Ã¢â‚¬ËœÃ‚Â/ÃƒÂ°Ã…Â¸Ã¢â‚¬ËœÃ…Â½) under assistant answers only.
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
                    // instant the pointer touched it ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â a per-frame show/hide
                    // flicker ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â and egui drops the pending click when the
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
                                egui::RichText::new("ÃƒÂ¢Ã‚Â§Ã¢â‚¬Â°").color(theme.text_dim).size(11.0),
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
                    commit_message_edit(&mut self.sessions, &mut self.chat_area, index);
                }
                if i.key_pressed(egui::Key::Escape) {
                    if let Some(sid) = &self.sessions.selected_session_id {
                        if let Some(runtime) = self.sessions.session_store.get_mut(sid) {
                            runtime.chat_state.editing_message_index = None;
                            runtime.chat_state.editing_message_content.clear();
                        }
                    }
                }
            });
        }
    }

}

pub(super) fn commit_message_edit(sessions: &mut SessionState, chat: &mut ChatArea, index: usize) {
    let new_content = match &sessions.selected_session_id {
        Some(sid) => sessions
            .session_store
            .get(sid)
            .map(|r| r.chat_state.editing_message_content.clone()),
        None => return,
    };
    let new_content = match new_content {
        Some(c) => c,
        None => return,
    };

    // Update chat_display
    if let Some(sid) = &sessions.selected_session_id {
        if let Some(runtime) = sessions.session_store.get_mut(sid) {
            if index < runtime.chat_state.messages.len() {
                runtime.chat_state.messages[index].content = new_content.clone();
            }
        }
    }

    // Update the underlying client conversation, then persist via the
    // single save path.
    if let Some(sid) = sessions.selected_session_id.clone() {
        if let Some(runtime) = sessions.session_store.get(&sid) {
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
        if let Some(rt) = sessions.session_store.get(&sid) {
            if let Err(e) = rt.client.save_session() {
                tracing::warn!(error = %e, "Failed to save session after edit");
            }
        }
    }

    // Clear edit state
    if let Some(sid) = &sessions.selected_session_id {
        if let Some(runtime) = sessions.session_store.get_mut(sid) {
            runtime.chat_state.editing_message_index = None;
            runtime.chat_state.editing_message_content.clear();
        }
    }
    // In-place edit keeps the message count unchanged, so force the display
    // snapshot to rebuild next frame (the len-based check would miss it).
    chat.display_dirty = true;
}

pub(super) fn delete_message(sessions: &mut SessionState, index: usize) {
    if let Some(sid) = sessions.selected_session_id.clone() {
        if let Some(runtime) = sessions.session_store.get_mut(&sid) {
            if index < runtime.chat_state.messages.len() {
                runtime.chat_state.messages.remove(index);
                // S2: keep the feedback state index-aligned after deletion
                // (keys below stay, the deleted one drops, higher ones shift).
                runtime.chat_state.message_ratings =
                    std::mem::take(&mut runtime.chat_state.message_ratings)
                        .into_iter()
                        .map(|(k, v)| (if k > index { k - 1 } else { k }, v))
                        .collect();
                runtime.chat_state.feedback_comment_for =
                    match runtime.chat_state.feedback_comment_for {
                        Some(f) if f == index => None,
                        Some(f) if f > index => Some(f - 1),
                        other => other,
                    };
                // Also remove from the underlying client conversation ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â but only
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
        if let Some(rt) = sessions.session_store.get(&sid) {
            if let Err(e) = rt.client.save_session() {
                tracing::warn!(error = %e, "Failed to save session after delete");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_thinking_tags_removes_wrapper() {
        assert_eq!(strip_thinking_tags("<think> Hello</think>"), "Hello");
        assert_eq!(strip_thinking_tags("no tags at all"), "no tags at all");
        assert_eq!(strip_thinking_tags("<think> spaced body</think>"), "spaced body");
        // Prefix present but no closing tag -> only the prefix is stripped.
        assert_eq!(strip_thinking_tags("<think> partial"), "partial");
        assert_eq!(strip_thinking_tags(""), "");
    }

    #[test]
    fn display_content_ref_borrows_when_plain() {
        let s = "plain text, no tags";
        let cow = display_content_ref(s);
        assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
        assert_eq!(cow.as_ptr(), s.as_ptr());
        assert_eq!(&*cow, s);
    }

    #[test]
    fn display_content_ref_strips_when_tagged() {
        let s = "<think> reasoning</think>";
        let cow = display_content_ref(s);
        assert!(matches!(cow, std::borrow::Cow::Owned(_)));
        assert_eq!(&*cow, "reasoning");
    }
}
