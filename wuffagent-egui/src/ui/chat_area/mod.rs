use std::collections::HashMap;

use eframe::egui;

use wuffagent_core::memory::MemoryManager;
use wuffagent_core::types::ChatMessage;

use super::state::ChatApp;
use super::state::groups::SessionState;
use super::theme::Theme;

// ── TEMPORARY layout debugging (set WUFF_LAYOUT_DBG=1 to enable) ────────
static LAYOUT_DBG_FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn layout_dbg_enabled() -> bool {
    std::env::var_os("WUFF_LAYOUT_DBG").is_some()
        && (2..7).contains(&(LAYOUT_DBG_FRAME.load(std::sync::atomic::Ordering::Relaxed) % 1000))
}
// ────────────────────────────────────────────────────────────────────────

mod bubbles;
mod markdown;
mod tool_cards;
mod tool_json;

/// Chat-area VIEW state (the panel-VM half of the chat column): the cached
/// message / streaming snapshots rebuilt from the session store each frame
/// plus their staleness keys, and the `display_dirty` flag set by in-place
/// message edits. Owned by [`ChatApp::chat_area`], so the chat column's
/// draw code reads/writes `self.chat_area.*` instead of the god-struct.
///
/// Per-session chat RUNTIME (messages, generation state, scroll state) stays
/// in `wuffagent_core::sessions::SessionRuntime` — this struct only caches
/// what the chat view renders.
pub struct ChatArea {
    /// Cached message list for the displayed session, rebuilt only when the
    /// displayed session changed or the message count changed (append /
    /// remove). Shared as an `Arc` so a steady-state frame is an `Arc::clone`
    /// instead of a deep clone of every message (which copies large tool
    /// outputs + base64 images and is the main lag).
    pub display_snapshot: std::sync::Arc<Vec<ChatMessage>>,
    /// Session ID the current `display_snapshot` holds.
    pub snapshot_session: Option<String>,
    /// Message count the current `display_snapshot` was built for.
    pub snapshot_len: usize,
    /// Set by in-place message edits (content changed, count unchanged);
    /// cleared when the next snapshot is rebuilt.
    pub display_dirty: bool,
    /// Cached streaming state `(current_thinking, stream_buffer, active_tools,
    /// is_generating)` for the displayed session. Rebuilt only when
    /// `stream_snapshot_key` changes, so a steady-state frame is an O(1)
    /// `Arc::clone` instead of a per-frame deep clone of the (multi-MB)
    /// growing thinking/stream buffers. Field 0 is the `(current_thinking,
    /// stream_buffer)` pair so draw code receives a `&(String, String)`
    /// unchanged.
    pub stream_snapshot:
        std::sync::Arc<((String, String), Vec<wuffagent_core::sessions::ActiveTool>, bool)>,
    /// Key the streaming snapshot was last built from (see [`stream_snapshot_key`]).
    pub stream_snapshot_key: StreamSnapshotKey,
}

/// Staleness key for the streaming snapshot:
/// `(session, is_generating, thinking_len, buffer_len, active_tools_revision)`.
/// The text buffers only GROW while generating (a reset passes through a
/// different length), so their lengths are a sufficient staleness key. The
/// live tool cards are keyed on the monotonically increasing revision because
/// their content can be REPLACED with same-length content (live output tail).
pub type StreamSnapshotKey = (Option<String>, bool, usize, usize, u64);

/// Build the streaming-snapshot key from the displayed session's state.
/// The text-buffer lengths are zeroed when not generating (the snapshot stores
/// empty buffers in that case).
pub fn stream_snapshot_key(
    session: &Option<String>,
    is_generating: bool,
    current_thinking_len: usize,
    stream_buffer_len: usize,
    active_tools_revision: u64,
) -> StreamSnapshotKey {
    (
        session.clone(),
        is_generating,
        if is_generating { current_thinking_len } else { 0 },
        if is_generating { stream_buffer_len } else { 0 },
        active_tools_revision,
    )
}

impl ChatArea {
    pub fn new() -> Self {
        Self {
            display_snapshot: std::sync::Arc::new(Vec::new()),
            snapshot_session: None,
            snapshot_len: 0,
            display_dirty: false,
            stream_snapshot: std::sync::Arc::new((
                (String::new(), String::new()),
                Vec::new(),
                false,
            )),
            stream_snapshot_key: (None, false, 0, 0, 0),
        }
    }

    /// Rebuild the cached message list for the displayed session if it is
    /// stale (the displayed session changed, an in-place edit set
    /// `display_dirty`, or the message count changed — append/remove) and
    /// return a shared `Arc` to it. Otherwise reuse it: a per-frame redraw
    /// is then an O(1) `Arc::clone` rather than a deep clone of every
    /// message (which copies large tool outputs + base64 images and is the
    /// main lag).
    pub fn refresh_display_snapshot(
        &mut self,
        sessions: &HashMap<String, wuffagent_core::sessions::SessionRuntime>,
        displayed_session_id: Option<&str>,
    ) -> std::sync::Arc<Vec<ChatMessage>> {
        let selected = displayed_session_id.map(|s| s.to_string());
        let current_len = selected
            .as_deref()
            .and_then(|sid| sessions.get(sid))
            .map(|r| r.chat_state.messages.len())
            .unwrap_or(0);
        if selected != self.snapshot_session
            || self.display_dirty
            || current_len != self.snapshot_len
        {
            let msgs = selected
                .as_deref()
                .and_then(|sid| sessions.get(sid))
                .map(|r| r.chat_state.messages.clone())
                .unwrap_or_default();
            self.snapshot_len = msgs.len();
            // The snapshot only records which session it holds; the caller
            // keeps its own `selected` for the rest of the frame (error
            // card, streaming, scroll state).
            self.snapshot_session = selected;
            self.display_snapshot = std::sync::Arc::new(msgs);
            self.display_dirty = false;
        }
        self.display_snapshot.clone()
    }

    /// Rebuild the cached streaming-state snapshot for the displayed session
    /// if its staleness key changed and return a shared `Arc` to it.
    ///
    /// The snapshot is a deep clone of the (multi-MB) growing
    /// thinking/stream buffers plus the live tool cards — see
    /// [`StreamSnapshotKey`] for the staleness key and why a steady-state
    /// frame is an O(1) `Arc::clone`.
    pub fn refresh_stream_snapshot(
        &mut self,
        sessions: &HashMap<String, wuffagent_core::sessions::SessionRuntime>,
        displayed_session_id: Option<&str>,
    ) -> std::sync::Arc<((String, String), Vec<wuffagent_core::sessions::ActiveTool>, bool)> {
        let selected = displayed_session_id.map(|s| s.to_string());
        let key = {
            let (is_gen, t_len, b_len, t_rev) = selected
                .as_deref()
                .and_then(|sid| sessions.get(sid))
                .map(|r| {
                    (
                        r.chat_state.is_generating,
                        r.chat_state.current_thinking.len(),
                        r.chat_state.stream_buffer.len(),
                        r.chat_state.active_tools_revision,
                    )
                })
                .unwrap_or((false, 0, 0, 0));
            stream_snapshot_key(&selected, is_gen, t_len, b_len, t_rev)
        };
        if self.stream_snapshot_key != key {
            let snap = selected
                .as_deref()
                .and_then(|sid| sessions.get(sid))
                .map(|r| {
                    let gen = r.chat_state.is_generating;
                    (
                        (
                            if gen {
                                r.chat_state.current_thinking.clone()
                            } else {
                                String::new()
                            },
                            if gen {
                                r.chat_state.stream_buffer.clone()
                            } else {
                                String::new()
                            },
                        ),
                        r.chat_state.active_tools.clone(),
                        gen,
                    )
                })
                .unwrap_or_default();
            self.stream_snapshot = std::sync::Arc::new(snap);
            self.stream_snapshot_key = key;
        }
        self.stream_snapshot.clone()
    }
}

// ── Pure display helpers (extracted from `impl ChatApp`; unit-tested below) ──

/// Human label for a `YYYY-MM-DD` day string.
fn day_label(day: &str) -> String {
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let yesterday = (now - chrono::TimeDelta::days(1))
        .format("%Y-%m-%d")
        .to_string();
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

/// Small per-tool glyph for the tool card headers.
fn tool_icon(name: &str) -> &'static str {
    match name {
        "shell" => "⚡",
        "read_file" => "📄",
        "write_file" | "append_file" | "apply_diff" | "replace_lines" => "✏️",
        "list_dir" | "mkdir" => "📁",
        "search_files" | "search_content" => "🔍",
        "copy" | "move" | "delete" | "file_info" => "📦",
        "web_search" => "🌐",
        "fetch_url" => "🔗",
        "show_image" => "🖼️",
        "calculation" => "🧮",
        "time" => "🕐",
        "save_memory"
        | "update_memory"
        | "search_memory"
        | "consolidate_memories"
        | "delete_memory" => "🧠",
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

/// Threshold in pixels to consider the user as "at bottom"
const SCROLL_BOTTOM_THRESHOLD: f32 = 10.0;

/// Core at-bottom check on plain values — unit-testable without an egui
/// context (the egui `ScrollAreaOutput`/`State` fields are private, so the
/// struct can't be constructed in a test; this keeps the logic testable).
fn is_at_bottom_from_values(
    content_height: f32,
    viewport_height: f32,
    current_offset: f32,
) -> bool {
    // If content fits in viewport, no scrolling needed - at bottom
    if content_height <= viewport_height {
        return true;
    }
    let max_offset = content_height - viewport_height;
    current_offset >= max_offset - SCROLL_BOTTOM_THRESHOLD
}

/// Check if the scroll area is at the bottom using ScrollAreaOutput after render.
fn is_at_bottom_from_output(
    output: &egui::containers::scroll_area::ScrollAreaOutput<()>,
) -> bool {
    is_at_bottom_from_values(
        output.content_size.y,
        output.inner_rect.height(),
        output.state.offset.y,
    )
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
        fonts
            .layout_no_wrap(SAMPLE.to_string(), font_id.clone(), egui::Color32::WHITE)
            .rect
            .width()
    });
    (width / SAMPLE.len() as f32).max(1.0)
}

/// Draw a small round avatar ("U" for user, "AI" for assistant): a filled
/// circle with a short label. Pure — depends only on the painter + theme, so
/// it is a module-level free fn (no `ChatApp` in sight).
pub(super) fn draw_avatar(ui: &mut egui::Ui, theme: &Theme, is_user: bool, size: f32) {
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(size, size));
    let color = if is_user { theme.primary } else { theme.accent };
    let label = if is_user { "U" } else { "AI" };
    let font_size = if is_user { size * 0.42 } else { size * 0.34 };
    ui.painter()
        .rect(
            rect,
            7.0,
            color,
            egui::Stroke::NONE,
            egui::StrokeKind::Middle,
        );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::new(font_size, egui::FontFamily::Proportional),
        egui::Color32::WHITE,
    );
    ui.allocate_space(egui::vec2(size, size));
}

/// Themed code block: theme background, 1px border, uniform padding. Pure —
/// takes a closure to add the contents, so it is a module-level free fn.
pub(super) fn code_block(
    ui: &mut egui::Ui,
    theme: &Theme,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::NONE
        .fill(theme.code_bg)
        .stroke(egui::Stroke::new(1.0, theme.code_border))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(8))
        .show(ui, add_contents);
}

impl ChatApp {

    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        LAYOUT_DBG_FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let theme = Theme::from_name(&self.core.config.theme);

        // Sub-session tab bar (main tab + one tab per open sub-session).
        // Only shown while at least one sub-session tab is open, so plain
        // sessions keep their current look.
        if self.displayed_session_id().is_some() && !self.sessions.sub_session_tabs.is_empty() {
            draw_sub_session_tabs(ui, &theme, &mut self.sessions);
        }

        // Get the current session's chat state, or show empty state.
        // The shared message snapshot lives in the chat-area panel state and
        // is rebuilt only when stale (displayed session changed, in-place
        // edit set `display_dirty`, message count changed) — see
        // `ChatArea::refresh_display_snapshot`. Re-key on the DISPLAYED
        // session (active sub-session tab, else the selected one) so the tab
        // bar swaps the chat area per tab.
        let selected = self.displayed_session_id().map(|s| s.to_string());
        let messages = self
            .chat_area
            .refresh_display_snapshot(&self.sessions.session_store, selected.as_deref());

        // Show pending error as a subtle red-tinted card (from the current session)
        if let Some(sid) = &selected {
            if let Some(runtime) = self.sessions.session_store.get(sid) {
                if let Some(err) = &runtime.chat_state.pending_error {
                    egui::Frame::NONE
                        .fill(theme.error.linear_multiply(0.12))
                        .stroke(egui::Stroke::new(1.0, theme.error.linear_multiply(0.35)))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.add(breaking_label(
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
        // Served from a shared `Arc` (in the chat-area panel state) that is
        // only rebuilt when its staleness key changes — see
        // `ChatArea::refresh_stream_snapshot` — a steady-state frame is an
        // O(1) `Arc::clone` instead of a per-frame deep clone of the
        // (multi-MB) growing thinking/stream buffers plus the live tool cards.
        let stream_snapshot = self
            .chat_area
            .refresh_stream_snapshot(&self.sessions.session_store, selected.as_deref());
        let streaming = &stream_snapshot.0;
        let is_streaming = stream_snapshot.2;
        let active_tools = &stream_snapshot.1;

        // Stick to bottom when the user is already there or forced the button.
        let scroll_output = egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, true])
            .stick_to_bottom(
                selected
                    .as_deref()
                    .map(|sid| {
                        self.sessions
                            .session_store
                            .get(sid)
                            .map(|r| r.chat_state.scroll_to_bottom_requested)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
                    || selected
                        .as_deref()
                        .map(|sid| {
                            self.sessions
                                .session_store
                                .get(sid)
                                .map(|r| r.chat_state.at_bottom)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false),
            )
            .show(ui, |ui| {
                // Bubbles dock edge-to-edge across the full chat width, like
                // the input field: no centered max-width column. (In egui 0.36
                // `horizontal_centered` only centers VERTICALLY, and a plain
                // scope shrinks to the widest content's desired width — so
                // each row stretches to the available width itself; see
                // draw_message / draw_tool_card / draw_streaming_line.)
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    if layout_dbg_enabled() {
                        eprintln!(
                            "[ldbg] list avail_w={:.1} max_rect={:?}",
                            ui.available_width(),
                            ui.max_rect()
                        );
                    }
                    if messages.is_empty() && !is_streaming {
                        Self::draw_empty_state(ui, &theme);
                        return;
                    }
                    let mut prev_day: Option<&str> = None;
                    for (i, msg) in messages.iter().enumerate() {
                        if let Some(day) =
                            wuffagent_core::types::timestamp_day(&msg.timestamp)
                        {
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
                        bubbles::draw_streaming_line(ui, &theme, streaming);
                    }
                    // Live tool cards for calls that are executing right now.
                    for tool in active_tools {
                        bubbles::draw_active_tool_card(ui, tool, &theme);
                    }
                    ui.add_space(12.0);
                });
            });

        // Update scroll state for the current session.
        // Compute `at_bottom` (immutable self borrow) before mutating the store.
        let at_bottom = is_at_bottom_from_output(&scroll_output);
        if let Some(sid) = &selected {
            if let Some(runtime) = self.sessions.session_store.get_mut(sid) {
                runtime.chat_state.scroll_to_bottom_requested = false;
                runtime.chat_state.at_bottom = at_bottom;

                // Update button visibility and opacity
                if runtime.chat_state.at_bottom {
                    runtime.chat_state.button_opacity =
                        (runtime.chat_state.button_opacity * 0.85).max(0.0);
                    if runtime.chat_state.button_opacity < 0.01 {
                        runtime.chat_state.button_visible = false;
                    }
                } else {
                    runtime.chat_state.button_visible = true;
                    runtime.chat_state.button_opacity =
                        (runtime.chat_state.button_opacity + 0.12).min(1.0);
                }
            }
        }

        // Show scroll-to-bottom button when not at bottom and opacity > 0.
        // Snapshot the opacity first so we can call an `&mut self` helper.
        let (button_visible, button_opacity) = selected
            .as_deref()
            .and_then(|sid| self.sessions.session_store.get(sid))
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
            ui.label(
                egui::RichText::new("Start a conversation")
                    .color(theme.text_primary)
                    .strong()
                    .size(16.0),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Ask a question or give the agent a task.")
                    .color(theme.text_dim)
                    .size(12.0),
            );
        });
    }

    /// Centered divider ("Today" / "Yesterday" / date) between message groups
    /// that cross a day boundary.
    fn draw_date_separator(ui: &mut egui::Ui, day: &str, theme: &Theme) {
        let label = day_label(day);
        ui.add_space(10.0);
        let (row_rect, _resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(0.0), 16.0),
            egui::Sense::hover(),
        );
        let font = egui::FontId::new(10.0, egui::FontFamily::Proportional);
        let galley = ui
            .ctx()
            .fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), theme.text_dim));
        let half_gap = galley.rect.width() / 2.0 + 12.0;
        let line_y = row_rect.center().y;
        let stroke = egui::Stroke::new(1.0, theme.divider);
        ui.painter().hline(
            row_rect.left()..=(row_rect.center().x - half_gap),
            line_y,
            stroke,
        );
        ui.painter().hline(
            (row_rect.center().x + half_gap)..=row_rect.right(),
            line_y,
            stroke,
        );
        ui.painter().text(
            row_rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            font,
            theme.text_dim,
        );
        ui.add_space(6.0);
    }

    /// Rounded-square avatar with a letter label, allocated in row flow.
    fn draw_scroll_to_bottom_button(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        button_opacity: f32,
    ) {
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
                alpha,
            );
            let border_color = egui::Color32::from_rgba_premultiplied(
                theme.border.r(),
                theme.border.g(),
                theme.border.b(),
                alpha,
            );
            let scroll_btn = egui::Button::new(
                egui::RichText::new("↓")
                    .color(theme.text_primary)
                    .size(15.0),
            )
            .fill(fill_color)
            .stroke(egui::Stroke::new(1.0, border_color))
            .corner_radius(18);
            if ui.add(scroll_btn).clicked() {
                // Trigger auto-scroll on next frame (for the displayed session)
                if let Some(sid) = self.displayed_session_id().map(|s| s.to_string()) {
                    if let Some(runtime) = self.sessions.session_store.get_mut(&sid) {
                        runtime.chat_state.scroll_to_bottom_requested = true;
                    }
                }
            }
        });
    }

}

/// S2: 👍/👎 feedback row under an assistant answer.
///
/// 👍 saves immediately; 👎 opens a one-line optional comment with
/// save (✓) / cancel (✕). A rated message shows the chosen button
/// highlighted with both buttons disabled (no double-save).
fn draw_feedback_row(sessions: &mut SessionState, memory: &MemoryManager, ui: &mut egui::Ui, index: usize, theme: &Theme) {
    let Some(sid) = sessions.active_tab.as_deref().or(sessions.selected_session_id.as_deref()).map(|s| s.to_string()) else {
        return;
    };
    let (rated, comment_open, mut comment) = match sessions.session_store.get(&sid) {
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
                egui::Button::new(egui::RichText::new("👍").size(if is_good {
                    13.0
                } else {
                    11.0
                }))
                .fill(if is_good {
                    theme.primary
                } else {
                    theme.hover_bg
                })
                .corner_radius(4),
            )
            .clicked()
        {
            save_good = true;
        }
        if ui
            .add_enabled(
                rated.is_none(),
                egui::Button::new(egui::RichText::new("👎").size(if is_bad {
                    13.0
                } else {
                    11.0
                }))
                .fill(if is_bad {
                    theme.primary
                } else {
                    theme.hover_bg
                })
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
                    egui::Button::new(
                        egui::RichText::new("✓").color(theme.text_dim).size(11.0),
                    )
                    .fill(theme.success)
                    .corner_radius(4),
                )
                .clicked()
            {
                save_bad = true;
            }
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("✕").color(theme.text_dim).size(11.0),
                    )
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
        if let Some(rt) = sessions.session_store.get_mut(&sid) {
            rt.chat_state.feedback_comment_for = None;
            rt.chat_state.feedback_comment.clear();
        }
    }
    if save_good {
        super::chat_feedback::save_message_feedback(sessions, memory, index, true, "");
    } else if save_bad {
        super::chat_feedback::save_message_feedback(sessions, memory, index, false, &comment);
    } else if open_bad {
        if let Some(rt) = sessions.session_store.get_mut(&sid) {
            rt.chat_state.feedback_comment_for = Some(index);
            rt.chat_state.feedback_comment.clear();
        }
    } else if comment_open {
        // Field still open after this frame (no save/cancel): persist the
        // typed comment. On a failed save the field stays open for retry.
        if let Some(rt) = sessions.session_store.get_mut(&sid) {
            if rt.chat_state.feedback_comment_for == Some(index) {
                rt.chat_state.feedback_comment = comment;
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use wuffagent_core::agents::{AgentEngine, ChatClientAdapter, ChatPipeline, LlmClient};
    use wuffagent_core::client::{ChatClient, ConnectionSettings};
    use wuffagent_core::tools::ToolManager;
    use wuffagent_core::types::ReasoningMode;

    /// Minimal in-memory `SessionRuntime` for exercising the snapshot refresh
    /// logic (no server contacted, no session file touched).
    fn test_runtime(session_id: &str) -> wuffagent_core::sessions::SessionRuntime {
        let client = std::sync::Arc::new(ChatClient::from_settings(
            ConnectionSettings::new("http://127.0.0.1:9999", None),
        ));
        let llm: std::sync::Arc<dyn LlmClient> =
            std::sync::Arc::new(ChatClientAdapter::new(client.as_ref().clone()));
        let engine = AgentEngine::new(
            llm,
            std::sync::Arc::new(ToolManager::new_empty()),
            client.clone(),
        );
        let (tx, _rx) = std::sync::mpsc::channel();
        let pipeline = ChatPipeline::new(
            std::sync::Arc::new(engine.clone()),
            tx,
            ReasoningMode::default(),
            session_id.to_string(),
        );
        wuffagent_core::sessions::SessionRuntime::new(
            session_id.to_string(),
            format!("test-{session_id}"),
            client,
            pipeline,
            engine,
            tokio_util::sync::CancellationToken::new(),
        )
    }

    fn sessions_with(id: &str) -> HashMap<String, wuffagent_core::sessions::SessionRuntime> {
        let mut m = HashMap::new();
        m.insert(id.to_string(), test_runtime(id));
        m
    }

    fn user_msg(content: &str) -> wuffagent_core::types::ChatMessage {
        wuffagent_core::types::ChatMessage {
            role: "user".to_string(),
            content: content.to_string(),
            timestamp: String::new(),
            image: None,
            kind: wuffagent_core::types::MessageKind::Normal,
        }
    }

    // ---- stream_snapshot_key ----

    #[test]
    fn stream_snapshot_key_stable_when_state_unchanged() {
        let session = Some("s1".to_string());
        let a = stream_snapshot_key(&session, true, 10, 20, 3);
        let b = stream_snapshot_key(&session, true, 10, 20, 3);
        assert_eq!(a, b);
    }

    #[test]
    fn stream_snapshot_key_detects_each_single_change() {
        let session = Some("s1".to_string());
        let base = stream_snapshot_key(&session, true, 10, 20, 3);
        // Session switch (or to none).
        let other = Some("s2".to_string());
        assert_ne!(stream_snapshot_key(&other, true, 10, 20, 3), base);
        let none = None;
        assert_ne!(stream_snapshot_key(&none, true, 10, 20, 3), base);
        // A new chunk grows one of the buffers.
        assert_ne!(stream_snapshot_key(&session, true, 11, 20, 3), base);
        assert_ne!(stream_snapshot_key(&session, true, 10, 21, 3), base);
        // A tool-card mutation bumps the revision (catches same-length
        // content replacement, which lengths alone would miss).
        assert_ne!(stream_snapshot_key(&session, true, 10, 20, 4), base);
        // The generating flag flips while all lengths are zero.
        assert_ne!(
            stream_snapshot_key(&session, false, 0, 0, 3),
            stream_snapshot_key(&session, true, 0, 0, 3)
        );
    }

    #[test]
    fn stream_snapshot_key_zeroes_text_lengths_when_not_generating() {
        let session = Some("s1".to_string());
        assert_eq!(
            stream_snapshot_key(&session, false, 99, 99, 5),
            (Some("s1".to_string()), false, 0, 0, 5)
        );
    }

    // ---- refresh_display_snapshot ----

    #[test]
    fn display_snapshot_rebuilds_on_dirty_len_change_and_reuse() {
        let mut area = ChatArea::new();
        area.display_dirty = true; // mirrors ChatApp::new (first-frame build)
        let mut sessions = sessions_with("s1");
        sessions
            .get_mut("s1")
            .unwrap()
            .chat_state
            .messages
            .push(user_msg("hi"));

        let a1 = area.refresh_display_snapshot(&sessions, Some("s1"));
        assert_eq!(a1.len(), 1);
        assert_eq!(area.snapshot_session.as_deref(), Some("s1"));
        assert!(!area.display_dirty);

        // Unchanged session → same `Arc` reused (no deep clone).
        let a2 = area.refresh_display_snapshot(&sessions, Some("s1"));
        assert!(std::sync::Arc::ptr_eq(&a1, &a2));

        // In-place edit (dirty flag, count unchanged) → rebuild.
        area.display_dirty = true;
        let a3 = area.refresh_display_snapshot(&sessions, Some("s1"));
        assert!(!std::sync::Arc::ptr_eq(&a2, &a3));
        assert!(!area.display_dirty);

        // Message append (count change, no dirty flag) → rebuild.
        sessions
            .get_mut("s1")
            .unwrap()
            .chat_state
            .messages
            .push(user_msg("again"));
        let a4 = area.refresh_display_snapshot(&sessions, Some("s1"));
        assert!(!std::sync::Arc::ptr_eq(&a3, &a4));
        assert_eq!(a4.len(), 2);

        // Session switch → rebuild from the other session.
        sessions.insert("s2".to_string(), test_runtime("s2"));
        let a5 = area.refresh_display_snapshot(&sessions, Some("s2"));
        assert!(!std::sync::Arc::ptr_eq(&a4, &a5));
        assert_eq!(a5.len(), 0);
        assert_eq!(area.snapshot_session.as_deref(), Some("s2"));
    }

    #[test]
    fn display_snapshot_empty_for_missing_session() {
        let mut area = ChatArea::new();
        area.display_dirty = true;
        let sessions: HashMap<String, wuffagent_core::sessions::SessionRuntime> =
            HashMap::new();
        let a = area.refresh_display_snapshot(&sessions, Some("nope"));
        assert!(a.is_empty());
        assert_eq!(area.snapshot_session.as_deref(), Some("nope"));
    }

    // ---- refresh_stream_snapshot ----

    #[test]
    fn stream_snapshot_reuse_and_rebuild_on_state_changes() {
        let mut area = ChatArea::new();
        let mut sessions = sessions_with("s1");

        // Not generating → empty buffers, `is_generating = false`.
        let r1 = area.refresh_stream_snapshot(&sessions, Some("s1"));
        assert!(!r1.2);
        assert!(r1.0 .0.is_empty() && r1.0 .1.is_empty());

        // Unchanged → same `Arc` reused.
        let r2 = area.refresh_stream_snapshot(&sessions, Some("s1"));
        assert!(std::sync::Arc::ptr_eq(&r1, &r2));

        // Generation starts with content → rebuild, buffers cloned.
        {
            let s = sessions.get_mut("s1").unwrap();
            s.chat_state.is_generating = true;
            s.chat_state.current_thinking.push_str("thinking...");
            s.chat_state.stream_buffer.push_str("hello");
        }
        let r3 = area.refresh_stream_snapshot(&sessions, Some("s1"));
        assert!(!std::sync::Arc::ptr_eq(&r2, &r3));
        assert!(r3.2);
        assert_eq!(r3.0 .0, "thinking...");
        assert_eq!(r3.0 .1, "hello");

        // Steady state (no new text) → reuse again.
        let r4 = area.refresh_stream_snapshot(&sessions, Some("s1"));
        assert!(std::sync::Arc::ptr_eq(&r3, &r4));

        // A streamed chunk grows the buffer → rebuild.
        sessions
            .get_mut("s1")
            .unwrap()
            .chat_state
            .stream_buffer
            .push_str(" world");
        let r5 = area.refresh_stream_snapshot(&sessions, Some("s1"));
        assert!(!std::sync::Arc::ptr_eq(&r4, &r5));
        assert_eq!(r5.0 .1, "hello world");

        // Generation ends → rebuild, buffers cleared.
        sessions.get_mut("s1").unwrap().chat_state.is_generating = false;
        let r6 = area.refresh_stream_snapshot(&sessions, Some("s1"));
        assert!(!std::sync::Arc::ptr_eq(&r5, &r6));
        assert!(!r6.2);
        assert!(r6.0 .0.is_empty() && r6.0 .1.is_empty());
    }

    #[test]
    fn stream_snapshot_empty_for_missing_session() {
        let mut area = ChatArea::new();
        let sessions: HashMap<String, wuffagent_core::sessions::SessionRuntime> =
            HashMap::new();
        let r = area.refresh_stream_snapshot(&sessions, Some("nope"));
        assert!(!r.2);
        assert!(r.0 .0.is_empty() && r.0 .1.is_empty());
        assert!(r.1.is_empty());
    }

    // ── Pure display helpers ─────────────────────────────────────────────
    #[test]
    fn format_duration_ms_seconds_minutes() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(42), "42ms");
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1000), "1.0s");
        assert_eq!(format_duration(1300), "1.3s");
        assert_eq!(format_duration(59_999), "60.0s");
        assert_eq!(format_duration(60_000), "1m 00s");
        assert_eq!(format_duration(125_000), "2m 05s");
        assert_eq!(format_duration(3_600_000), "60m 00s");
    }

    #[test]
    fn tool_icon_known_and_fallback() {
        assert_eq!(tool_icon("shell"), "⚡");
        assert_eq!(tool_icon("read_file"), "📄");
        assert_eq!(tool_icon("write_file"), "✏️");
        assert_eq!(tool_icon("append_file"), "✏️");
        assert_eq!(tool_icon("apply_diff"), "✏️");
        assert_eq!(tool_icon("replace_lines"), "✏️");
        assert_eq!(tool_icon("list_dir"), "📁");
        assert_eq!(tool_icon("mkdir"), "📁");
        assert_eq!(tool_icon("search_content"), "🔍");
        assert_eq!(tool_icon("search_files"), "🔍");
        assert_eq!(tool_icon("web_search"), "🌐");
        assert_eq!(tool_icon("fetch_url"), "🔗");
        assert_eq!(tool_icon("show_image"), "🖼️");
        assert_eq!(tool_icon("calculation"), "🧮");
        assert_eq!(tool_icon("time"), "🕐");
        assert_eq!(tool_icon("save_memory"), "🧠");
        assert_eq!(tool_icon("update_memory"), "🧠");
        assert_eq!(tool_icon("search_memory"), "🧠");
        assert_eq!(tool_icon("consolidate_memories"), "🧠");
        assert_eq!(tool_icon("delete_memory"), "🧠");
        assert_eq!(tool_icon("handoff"), "🔀");
        assert_eq!(tool_icon("restart"), "🔁");
        assert_eq!(tool_icon("copy"), "📦");
        assert_eq!(tool_icon("move"), "📦");
        assert_eq!(tool_icon("delete"), "📦");
        assert_eq!(tool_icon("file_info"), "📦");
        assert_eq!(tool_icon("some_unknown_tool"), "🔧");
    }

    #[test]
    fn day_label_today_yesterday_and_other() {
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let yesterday = (now - chrono::TimeDelta::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let other = (now - chrono::TimeDelta::days(10))
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(day_label(&today), "Today");
        assert_eq!(day_label(&yesterday), "Yesterday");
        let lbl = day_label(&other);
        assert_ne!(lbl, "Today");
        assert_ne!(lbl, "Yesterday");
        assert!(!lbl.is_empty());
        assert_eq!(day_label("not-a-date"), "not-a-date");
    }

    #[test]
    fn is_at_bottom_from_output_logic() {
        // Content fits in the viewport → always "at bottom".
        assert!(is_at_bottom_from_values(100.0, 200.0, 0.0));
        // Scrolled to the very bottom (offset == content - viewport = 300).
        assert!(is_at_bottom_from_values(500.0, 200.0, 300.0));
        // Within the 10px threshold of the bottom.
        assert!(is_at_bottom_from_values(500.0, 200.0, 295.0));
        // Mid-scroll → not at bottom.
        assert!(!is_at_bottom_from_values(500.0, 200.0, 100.0));
    }

    fn test_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(800.0, 600.0),
            )),
            ..Default::default()
        }
    }

    #[test]
    fn char_width_is_clamped_positive() {
        let ctx = egui::Context::default();
        let font_id = egui::FontId::proportional(16.0);
        let mut out = ctx.run_ui(test_input(), |ui| {
            let w = char_width(ui, &font_id);
            assert!(
                w >= 1.0,
                "char_width should be clamped to >= 1.0, got {w}"
            );
            assert!(w.is_finite(), "char_width should be finite, got {w}");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn breaking_label_lays_out_text() {
        let ctx = egui::Context::default();
        let font_id = egui::FontId::proportional(14.0);
        let mut out = ctx.run_ui(test_input(), |ui| {
            let fid = font_id.clone();
            let resp = ui.add(breaking_label(
                "hello world",
                fid.clone(),
                egui::Color32::WHITE,
                false,
            ));
            assert!(
                resp.rect.width() > 0.0,
                "label for non-empty text should have width, got {:?}",
                resp.rect
            );
            let empty = ui.add(breaking_label("", fid, egui::Color32::WHITE, false));
            assert!(
                empty.rect.width() <= 1.0,
                "empty label should have ~zero width, got {:?}",
                empty.rect
            );
        });
        out.textures_delta.clear();
    }
}

/// Tab bar for sub-sessions: the main tab (currently selected session)
/// plus one tab per open sub-session. Clicking a tab makes it the
/// displayed session (the chat area re-keys on it); the × on a sub-tab
/// closes it (the session file stays in the session list). A sub-tab shows
/// a running dot while its session's turn is generating.
fn draw_sub_session_tabs(ui: &mut egui::Ui, theme: &Theme, sessions: &mut SessionState) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.set_min_height(30.0);
        // Main tab (the currently selected session)
        let main_name = sessions
            .selected_session_id
            .as_ref()
            .and_then(|sid| sessions.session_store.get(sid))
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "Session".to_string());
        let main_active = sessions.active_tab.is_none();
        if ui
            .selectable_label(
                main_active,
                egui::RichText::new(main_name).color(if main_active {
                    theme.text_primary
                } else {
                    theme.text_dim
                }),
            )
            .clicked()
        {
            sessions.active_tab = None;
        }
        // One tab per open sub-session
        for sub_id in sessions.sub_session_tabs.clone() {
            let (label, generating) = sessions
                .session_store
                .get(&sub_id)
                .map(|r| (r.name.clone(), r.chat_state.is_generating))
                .unwrap_or_else(|| (sub_id.clone(), false));
            let active = sessions.active_tab.as_deref() == Some(sub_id.as_str());
            let mut text = label;
            if generating {
                text = format!("● {}", text);
            }
            if ui
                .selectable_label(
                    active,
                    egui::RichText::new(text).color(if active {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    }),
                )
                .clicked()
            {
                sessions.active_tab = Some(sub_id.clone());
            }
            let close = ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("×").size(12.0).color(theme.text_dim),
                    )
                    .min_size(egui::vec2(18.0, 18.0))
                    .fill(egui::Color32::TRANSPARENT),
                )
                .on_hover_text("Close tab (the session stays in the session list)");
            if close.clicked() {
                if sessions.active_tab.as_deref() == Some(sub_id.as_str()) {
                    sessions.active_tab = None;
                }
                sessions.sub_session_tabs.retain(|t| t != &sub_id);
            }
            ui.add_space(2.0);
        }
    });
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}
