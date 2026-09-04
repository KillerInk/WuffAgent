use eframe::egui;

use super::state::ChatApp;
use crate::types::{ChatMessage, MessageKind};
use super::theme::Theme;

impl ChatApp {
    /// Threshold in pixels to consider the user as "at bottom"
    const SCROLL_BOTTOM_THRESHOLD: f32 = 10.0;

    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);

        // Get the current session's chat state, or show empty state
        let messages = match &self.selected_session_id {
            Some(sid) => {
                if let Some(runtime) = self.session_store.get(sid) {
                    runtime.chat_state.messages.clone()
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        };

        // Show pending error as inline warning (from the current session)
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get(sid) {
                if let Some(err) = &runtime.chat_state.pending_error {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme.error, format!("⚠ Error: {}", err));
                    });
                    ui.separator();
                }
            }
        }

        // Snapshot streaming state up front so the scroll closure can call
        // `&mut self` helpers without holding an immutable borrow of the store.
        let streaming = self
            .selected_session_id
            .as_ref()
            .and_then(|sid| self.session_store.get(sid))
            .filter(|r| r.chat_state.is_generating)
            .map(|r| (r.chat_state.current_thinking.clone(), r.chat_state.stream_buffer.clone()))
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
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    for (i, msg) in messages.iter().enumerate() {
                        self.draw_message(ui, msg, i, &theme);
                    }
                    // Draw streaming line (values snapshotted before the scroll area).
                    self.draw_streaming_line(ui, &theme, &streaming);
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

    fn draw_scroll_to_bottom_button(&mut self, ui: &mut egui::Ui, theme: &Theme, button_opacity: f32) {
        let button_size = egui::vec2(36.0, 36.0);
        let button_pos = ui.max_rect().right_top() - egui::vec2(button_size.x + 16.0, 16.0);
        let button_rect = egui::Rect::from_min_size(button_pos, button_size);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(button_rect), |ui| {
            ui.set_max_size(button_size);
            ui.set_min_size(button_size);
            // Apply opacity via semi-transparent fill color (premultiplied alpha)
            let alpha = (button_opacity * 0.85 * 255.0) as u8;
            let fill_color = egui::Color32::from_rgba_premultiplied(
                theme.primary.r(),
                theme.primary.g(),
                theme.primary.b(),
                alpha
            );
            let scroll_btn = egui::Button::new("↓")
                .fill(fill_color)
                .rounding(18.0);
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

    /// Live streaming line shown while a response is in flight.
    /// Text renders as it arrives via StreamChunk events (no extra buffering).
    fn draw_streaming_line(&mut self, ui: &mut egui::Ui, theme: &Theme, streaming: &(String, String)) {
        let (current_thinking, stream_buffer) = streaming;
        let streaming_ts = chrono::Local::now().format("%H:%M:%S").to_string();
        ui.add_space(10.0);
        if !current_thinking.is_empty() {
            // Header row; the thinking text wraps on its own line below.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new(&streaming_ts)
                    .color(theme.text_dim)
                    .size(11.0));
                ui.colored_label(theme.text_dim, "Thinking:");
                ui.spinner();
            });
            ui.add(egui::Label::new(
                egui::RichText::new(current_thinking)
                    .color(theme.text_dim)
                    .italics()
                    .size(12.0)
            ).wrap());
        }
        if !stream_buffer.is_empty() {
            // Render the streamed text exactly like a completed AI message
            // (width-constrained bubble + wrapping label), so line breaks
            // match what the message looks like once it is committed.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new(&streaming_ts)
                    .color(theme.text_dim)
                    .size(11.0));
                ui.colored_label(theme.primary, "AI:");
                ui.spinner();
            });
            // Bubble row: reserve the avatar column exactly like `draw_message`
            // so the bubble's width and right edge match the committed messages.
            let avatar_size = 28.0;
            let avatar_margin = 16.0;
            let max_content_width = (ui.available_width() - avatar_size - avatar_margin * 2.0).max(120.0);
            ui.horizontal(|ui| {
                ui.add_space(avatar_size);
                ui.add_space(8.0); // gap between avatar and bubble
                ui.scope(|ui| {
                    ui.set_max_width(max_content_width);
                    ui.vertical(|ui| {
                        let bubble_frame = egui::Frame::none()
                            .fill(theme.surface_light)
                            .rounding(egui::Rounding::same(8.0))
                            .inner_margin(egui::Margin::same(6.0));
                        bubble_frame.show(ui, |ui| {
                            ui.add(egui::Label::new(
                                egui::RichText::new(stream_buffer)
                                    .color(theme.text_primary)
                            ).wrap());
                        });
                    });
                });
            });
        } else if current_thinking.is_empty() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new(&streaming_ts)
                    .color(theme.text_dim)
                    .size(11.0));
                ui.colored_label(theme.primary, "AI:");
                ui.spinner();
            });
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

    /// Strip <think>...</think> wrapper from thinking content for display.
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

        // Constrain content width (leaves room for avatar + margins)
        let avatar_size = 28.0;
        let avatar_margin = 16.0; // space from edges + gap to content
        let max_content_width = (ui.available_width() - avatar_size - avatar_margin * 2.0).max(120.0);

        // Message bubble backgrounds with good contrast
        let user_bubble_bg = egui::Color32::from_rgb(37, 99, 235); // dark blue for white text
        let bubble_bg = if is_user {
            user_bubble_bg
        } else if message.kind == MessageKind::Tool {
            theme.tool_bg
        } else {
            theme.surface_light
        };
        
        // Add spacing between messages
        ui.add_space(10.0);
        
        // Single horizontal layout: avatar | content
        ui.horizontal(|ui| {
            // Draw avatar
            let avatar_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(avatar_size, avatar_size));
            let avatar_color = if is_user { theme.primary } else { theme.accent };
            let avatar_label = if is_user { "U" } else { "AI" };
            let avatar_font = if is_user { 10.0 } else { 9.0 };
            
            ui.painter().circle(
                avatar_rect.center(),
                avatar_size / 2.0,
                avatar_color,
                egui::Stroke::NONE,
            );
            ui.painter().text(
                avatar_rect.center(),
                egui::Align2::CENTER_CENTER,
                avatar_label,
                egui::FontId::new(avatar_font, egui::FontFamily::Monospace),
                egui::Color32::WHITE,
            );
            
            // Reserve space for avatar so content doesn't overlap
            ui.allocate_space(egui::vec2(avatar_size, avatar_size));
            ui.add_space(8.0); // gap between avatar and bubble
            
            // Content column (bubble + timestamp)
            ui.scope(|ui| {
                ui.set_max_width(max_content_width);
                
                // Handle right-click context menu for edit/delete
                let response = ui.interact(ui.max_rect(), ui.id().with(index), egui::Sense::click());
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
                    // Message bubble
                    ui.scope(|ui| {
                        ui.visuals_mut().widgets.noninteractive.bg_fill = bubble_bg;
                        ui.style_mut().visuals.widgets.noninteractive.rounding = egui::Rounding::same(8.0);
                        ui.style_mut().visuals.widgets.inactive.rounding = egui::Rounding::same(8.0);
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                        
                        // Use a frame-like container for the bubble
                        let _bubble_inner_padding = egui::vec2(8.0, 6.0);
                        let bubble_frame = egui::Frame::none()
                            .fill(bubble_bg)
                            .rounding(egui::Rounding::same(8.0))
                            .inner_margin(egui::Margin::same(6.0));
                        
                        bubble_frame.show(ui, |ui| {
                            if is_editing {
                                if let Some(sid) = &self.selected_session_id {
                                    if let Some(runtime) = self.session_store.get_mut(sid) {
                                        ui.text_edit_multiline(&mut runtime.chat_state.editing_message_content);
                                    }
                                }
                            } else {
                                // Display image if present
                                if let Some(ref img_data) = message.image {
                                    if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, img_data) {
                                        let img = egui::Image::from_bytes("image", decoded);
                                        let max_img_width = (ui.available_width() - 10.0).max(50.0);
                                        ui.add(img.max_size(egui::Vec2::new(max_img_width, 300.0)));
                                    }
                                }
                                // Branch on message kind — no string-prefix sniffing
                                let text_color = if is_user {
                                    egui::Color32::WHITE
                                } else {
                                    theme.text_primary
                                };
                                if message.kind == MessageKind::Tool {
                                    self.draw_tool_message(ui, message, theme, index);
                                } else if message.kind == MessageKind::Thinking {
                                    // Thinking message — render dim and italic
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(&message.content)
                                            .color(theme.text_dim)
                                            .italics()
                                            .size(12.0)
                                    ).wrap());
                                } else {
                                    // Normal message — strip any legacy <think> tags
                                    let display_content = if message.content.contains("<think>") || message.content.contains("</think>") {
                                        Self::strip_thinking_tags(&message.content)
                                    } else {
                                        message.content.clone()
                                    };
                                    let content_label = egui::Label::new(
                                        egui::RichText::new(display_content)
                                            .color(text_color)
                                    ).wrap();
                                    ui.add(content_label);
                                    // Copy button for non-tool messages
                                    ui.add_space(4.0);
                                    let copy_btn = egui::Button::new("📋 Copy")
                                        .rounding(4.0)
                                        .sense(egui::Sense::click());
                                    if ui.add(copy_btn).clicked() {
                                        ui.ctx().copy_text(message.content.clone());
                                    }
                                }
                            }
                        });
                    });
                    
                    // Timestamp aligned under bubble content, not under avatar
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&message.timestamp)
                            .color(theme.text_dim)
                            .size(9.0));
                    });
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
        
        // Update session
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get(sid) {
                let cl = runtime.client.clone();
                let mut conv = cl.conversation().lock().unwrap();
                if index < conv.len() {
                    conv[index].content = new_content;
                }
                drop(conv);
                if let Err(e) = cl.save_session() {
                    eprintln!("Failed to save session after edit: {}", e);
                }
            }
        }
        
        // Clear edit state
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                runtime.chat_state.editing_message_index = None;
                runtime.chat_state.editing_message_content.clear();
            }
        }
    }

    /// Parse a tool message and render it with smart formatting.
    /// Content is "header||call_id||result" — or a bare result for messages
    /// loaded from older sessions (rendered without a header).
    fn draw_tool_message(&mut self, ui: &mut egui::Ui, message: &ChatMessage, theme: &Theme, msg_index: usize) {
        let parts: Vec<&str> = message.content.splitn(3, "||").collect();
        if parts.len() >= 2 {
            // Render header + call id
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(parts[0])
                    .color(theme.text_primary)
                    .size(11.0));
                ui.label(egui::RichText::new(parts[1])
                    .color(theme.text_dim)
                    .size(9.0));
            });
            ui.add_space(3.0);
        }
        let raw_result = if parts.len() > 2 { parts[2] } else { message.content.as_str() };

        // Render result with smart formatting
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(raw_result) {
            self.draw_tool_json_result(ui, &json, raw_result, theme, msg_index);
        } else {
            self.draw_tool_plain_result(ui, raw_result, theme);
        }
    }

    /// Render a tool result that is valid JSON with smart field extraction.
    fn draw_tool_json_result(&mut self, ui: &mut egui::Ui, json: &serde_json::Value, raw: &str, theme: &Theme, msg_index: usize) {
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
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(10, 10, 10))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::same(6.0))
                        .show(ui, |ui| {
                            for entry in display_entries {
                                if let Some(s) = entry.as_str() {
                                    ui.add(egui::Label::new(egui::RichText::new(s)
                                        .color(egui::Color32::from_rgb(180, 180, 180))
                                        .monospace()).wrap());
                                }
                            }
                            if entries.len() > max_entries {
                                ui.label(egui::RichText::new(format!("... and {} more entries", entries.len() - max_entries))
                                    .color(theme.text_dim)
                                    .size(10.0));
                            }
                        });
                }
            } else if is_file_read {
                // File read: show path badge + show/hide content button
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                    let char_count = content.len();
                    let is_expanded = self.selected_session_id.as_ref().map(|sid| {
                        self.session_store.get(sid).map(|r| r.chat_state.expanded_messages.contains(&msg_index)).unwrap_or(false)
                    }).unwrap_or(false);
                    let btn_text = if is_expanded {
                        format!("Hide content ({} chars)", char_count)
                    } else {
                        format!("Show content ({} chars)", char_count)
                    };
                    let btn = egui::Button::new(btn_text).rounding(4.0);
                    if ui.add(btn).clicked() {
                        if let Some(sid) = &self.selected_session_id {
                            if let Some(runtime) = self.session_store.get_mut(sid) {
                                if is_expanded {
                                    runtime.chat_state.expanded_messages.retain(|&i| i != msg_index);
                                } else {
                                    runtime.chat_state.expanded_messages.push(msg_index);
                                }
                            }
                        }
                    }
                    if is_expanded {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(10, 10, 10))
                            .rounding(4.0)
                            .inner_margin(egui::Margin::same(6.0))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(300.0)
                                    .show(ui, |ui| {
                                    ui.add(egui::Label::new(egui::RichText::new(content)
                                        .color(egui::Color32::from_rgb(200, 200, 200))
                                        .monospace()).wrap());
                                });
                            });
                    }
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
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(expr)
                            .color(theme.text_secondary)
                            .monospace());
                        ui.label(egui::RichText::new(" = ").color(theme.text_dim));
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

        // Copy button at the bottom
        ui.add_space(4.0);
        let copy_btn = egui::Button::new("📋 Copy")
            .rounding(4.0)
            .sense(egui::Sense::click());
        if ui.add(copy_btn).clicked() {
            ui.ctx().copy_text(raw.to_string());
        }
    }

    /// Render JSON as a key-value list.
    fn draw_tool_json_kv(&self, ui: &mut egui::Ui, json: &serde_json::Value, theme: &Theme) {
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(10, 10, 10))
            .rounding(4.0)
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                match json {
                    serde_json::Value::Object(map) => {
                        for (key, value) in map {
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new(egui::RichText::new(format!("{}:", key))
                                    .color(theme.text_secondary)
                                    .monospace()
                                    .size(11.0)).wrap());
                                let val_str = Self::json_value_to_string(value);
                                ui.add(egui::Label::new(egui::RichText::new(val_str)
                                    .color(egui::Color32::from_rgb(200, 200, 200))
                                    .monospace()
                                    .size(11.0)).wrap());
                            });
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            ui.add(egui::Label::new(egui::RichText::new(Self::json_value_to_string(item))
                                .color(egui::Color32::from_rgb(200, 200, 200))
                                .monospace()
                                .size(11.0)).wrap());
                        }
                    }
                    other => {
                        ui.add(egui::Label::new(egui::RichText::new(Self::json_value_to_string(other))
                            .color(egui::Color32::from_rgb(200, 200, 200))
                            .monospace()
                            .size(11.0)).wrap());
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

    /// Draw a clickable file path badge.
    fn draw_tool_path_badge(&self, ui: &mut egui::Ui, path: &str, theme: &Theme) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("📄").size(11.0));
            // Path as a clickable button
            let path_btn = egui::Button::new(egui::RichText::new(path)
                .color(theme.accent)
                .size(10.0)
                .monospace());
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
        });
    }

    /// Render a plain (non-JSON) tool result as a monospace code block.
    fn draw_tool_plain_result(&self, ui: &mut egui::Ui, text: &str, _theme: &Theme) {
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(10, 10, 10))
            .rounding(4.0)
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                let mut wrapped_text = text.to_string();
                // Pre-wrap long lines to avoid horizontal overflow
                let max_width = ui.available_width();
                if max_width > 0.0 {
                    let approx_chars_per_line = (max_width / 8.0).max(20.0) as usize; // monospace ~8px/char
                    if wrapped_text.len() > approx_chars_per_line {
                        let mut result = String::new();
                        let chars: Vec<char> = wrapped_text.chars().collect();
                        for chunk in chars.chunks(approx_chars_per_line) {
                            result.push_str(&chunk.iter().collect::<String>());
                            result.push('\n');
                        }
                        wrapped_text = result.trim_end().to_string();
                    }
                }
                ui.label(egui::RichText::new(wrapped_text)
                    .color(egui::Color32::from_rgb(200, 200, 200))
                    .monospace());
            });
        ui.add_space(4.0);
        let copy_btn = egui::Button::new("📋 Copy")
            .rounding(4.0)
            .sense(egui::Sense::click());
        if ui.add(copy_btn).clicked() {
            ui.ctx().copy_text(text.to_string());
        }
    }

    pub(super) fn delete_message(&mut self, index: usize) {
        if let Some(sid) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get_mut(sid) {
                if index < runtime.chat_state.messages.len() {
                    runtime.chat_state.messages.remove(index);
                }
                // Also remove from the underlying client conversation
                {
                    let cl = runtime.client.clone();
                    let mut conv = cl.conversation().lock().unwrap();
                    if index < conv.len() {
                        conv.remove(index);
                    }
                    drop(conv);
                    if let Err(e) = cl.save_session() {
                        eprintln!("Failed to save session after delete: {}", e);
                    }
                }
            }
        }
    }
}