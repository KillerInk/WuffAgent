use eframe::egui;

use super::state::ChatApp;
use crate::types::ChatMessage;
use super::theme::Theme;

impl ChatApp {
    /// Threshold in pixels to consider the user as "at bottom"
    const SCROLL_BOTTOM_THRESHOLD: f32 = 10.0;

    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
        
        // Draw agent pipeline panel at the top if active
        if self.chat.pipeline.active {
            self.draw_pipeline_panel(ui, &theme);
            ui.separator();
        }
        
        // Show pending error as inline warning
        let pending_error = self.chat.pending_error.take();
        if let Some(ref err) = pending_error {
            ui.horizontal(|ui| {
                ui.colored_label(theme.error, format!("⚠ Error: {}", err));
            });
            if ui.button("Dismiss").clicked() {
                // already taken above
            }
            ui.separator();
        }

        // Clone messages to avoid borrow checker issues
        let messages: Vec<ChatMessage> = self.chat.messages.clone();

        // Load egui's persisted scroll state BEFORE rendering to know where the user was
        let scroll_id = ui.id().with("chat_scroll");
        let prev_egui_state = egui::containers::scroll_area::State::load(ui.ctx(), scroll_id);
        
        // Determine if user was at bottom BEFORE this frame rendered new content
        // We compare the persisted state's offset against the previous frame's content height
        let was_at_bottom = self.compute_was_at_bottom(
            prev_egui_state.as_ref(),
            self.chat.prev_scroll_offset_y,
            self.chat.prev_content_height,
        );

        // Determine whether to auto-scroll this frame
        // Button click always scrolls to bottom; new messages only scroll if user was at bottom
        let auto_scroll = self.chat.scroll_to_bottom_requested || was_at_bottom;

        // Use egui's built-in scroll area with id_salt
        // When button clicked, pre-set offset to max so stick_to_bottom works even without new content
        let scroll_area = egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, true])
            .stick_to_bottom(auto_scroll);
        let scroll_output = if self.chat.scroll_to_bottom_requested {
            // Force scroll to bottom by setting offset to max content offset
            scroll_area
                .vertical_scroll_offset(self.chat.prev_content_height)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                        for (i, msg) in messages.iter().enumerate() {
                            self.draw_message(ui, msg, i, &theme);
                        }
                        let streaming_ts = chrono::Local::now().format("%H:%M:%S").to_string();
                        if self.chat.is_generating && !self.chat.current_response.is_empty() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(egui::RichText::new(&streaming_ts)
                                    .color(theme.text_dim)
                                    .size(11.0));
                                ui.colored_label(theme.primary, "AI:");
                                ui.add(egui::Label::new(
                                    egui::RichText::new(&self.chat.current_response)
                                        .color(theme.text_primary)
                                ));
                                ui.spinner();
                            });
                        } else if self.chat.is_generating && self.chat.current_response.is_empty() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(egui::RichText::new(&streaming_ts)
                                    .color(theme.text_dim)
                                    .size(11.0));
                                ui.colored_label(theme.primary, "AI:");
                                ui.spinner();
                            });
                        }
                    });
                })
        } else {
            scroll_area.show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    for (i, msg) in messages.iter().enumerate() {
                        self.draw_message(ui, msg, i, &theme);
                    }
                    let streaming_ts = chrono::Local::now().format("%H:%M:%S").to_string();
                    if self.chat.is_generating && !self.chat.current_response.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(egui::RichText::new(&streaming_ts)
                                .color(theme.text_dim)
                                .size(11.0));
                            ui.colored_label(theme.primary, "AI:");
                            ui.add(egui::Label::new(
                                egui::RichText::new(&self.chat.current_response)
                                    .color(theme.text_primary)
                            ));
                            ui.spinner();
                        });
                    } else if self.chat.is_generating && self.chat.current_response.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(egui::RichText::new(&streaming_ts)
                                .color(theme.text_dim)
                                .size(11.0));
                            ui.colored_label(theme.primary, "AI:");
                            ui.spinner();
                        });
                    }
                });
            })
        };

        // Reset the scroll-to-bottom flag after this frame
        self.chat.scroll_to_bottom_requested = false;

        // Update at_bottom from the output - use content_size vs offset from the rendered output
        self.chat.at_bottom = self.is_at_bottom_from_output(&scroll_output);
        
        // Save our tracking state so next frame has fresh data
        self.chat.prev_scroll_offset_y = scroll_output.state.offset.y;
        self.chat.scroll_offset_y = scroll_output.state.offset.y;
        self.chat.prev_content_height = scroll_output.content_size.y;

        // Update button visibility and opacity
        if self.chat.at_bottom {
            // Fade out button
            self.chat.button_opacity = (self.chat.button_opacity * 0.85).max(0.0);
            if self.chat.button_opacity < 0.01 {
                self.chat.button_visible = false;
            }
        } else {
            // Show button and fade in
            self.chat.button_visible = true;
            self.chat.button_opacity = (self.chat.button_opacity + 0.12).min(1.0);
        }

        // Show scroll-to-bottom button when not at bottom and opacity > 0
        if self.chat.button_visible && self.chat.button_opacity > 0.01 {
            self.draw_scroll_to_bottom_button(ui, &theme);
        }
    }

    fn draw_scroll_to_bottom_button(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let button_size = egui::vec2(36.0, 36.0);
        let button_pos = ui.max_rect().right_top() - egui::vec2(button_size.x + 16.0, 16.0);
        let button_rect = egui::Rect::from_min_size(button_pos, button_size);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(button_rect), |ui| {
            ui.set_max_size(button_size);
            ui.set_min_size(button_size);
            // Apply opacity via semi-transparent fill color (premultiplied alpha)
            let alpha = (self.chat.button_opacity * 0.85 * 255.0) as u8;
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
                self.chat.scroll_to_bottom_requested = true;
            }
        });
    }

    /// Compute whether user was at bottom using the persisted egui state before render.
    /// Compares the persisted scroll offset against the previous frame's content height.
    fn compute_was_at_bottom(
        &self,
        prev_state: Option<&egui::containers::scroll_area::State>,
        _prev_offset_y: f32,
        prev_content_height: f32,
    ) -> bool {
        // If we have no prior state (first render), assume at bottom
        if prev_state.is_none() {
            return true;
        }
        // If we have no prior content height (first render), assume at bottom
        if prev_content_height == 0.0 {
            return true;
        }
        // Use the at_bottom flag from last frame - it was computed correctly
        // from the ScrollAreaOutput after the previous render
        self.chat.at_bottom
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

    pub(super) fn draw_message(
        &mut self,
        ui: &mut egui::Ui,
        message: &ChatMessage,
        index: usize,
        theme: &Theme,
    ) {
        let is_user = message.role == "user";
        let is_editing = self.chat.editing_message_index == Some(index);
        
        // Constrain content width (leaves room for avatar + margins)
        let avatar_size = 28.0;
        let avatar_margin = 16.0; // space from edges + gap to content
        let max_content_width = (ui.available_width() - avatar_size - avatar_margin * 2.0).max(120.0);
        
        // Message bubble backgrounds with good contrast
        let user_bubble_bg = egui::Color32::from_rgb(37, 99, 235); // dark blue for white text
        let bubble_bg = if is_user {
            user_bubble_bg
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
                            self.chat.editing_message_index = Some(index);
                            self.chat.editing_message_content = message.content.clone();
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
                                ui.text_edit_multiline(&mut self.chat.editing_message_content);
                            } else {
                                // Display image if present
                                if let Some(ref img_data) = message.image {
                                    if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, img_data) {
                                        let img = egui::Image::from_bytes("image", decoded);
                                        let max_img_width = (ui.available_width() - 10.0).max(50.0);
                                        ui.add(img.max_size(egui::Vec2::new(max_img_width, 300.0)));
                                    }
                                }
                                // Message content with wrapping
                                // Tool messages: green/yellow tinted bubble
                                // User messages: white text on dark blue bubble for contrast
                                // AI messages: dark text on light bubble for contrast
                                let is_tool = message.role == "tool";
                                let _bubble_bg = if is_tool {
                                    theme.tool_bg
                                } else if is_user {
                                    theme.user_bg
                                } else {
                                    theme.ai_bg
                                };
                                let text_color = if is_user {
                                    egui::Color32::WHITE
                                } else if is_tool {
                                    theme.text_primary
                                } else {
                                    theme.text_primary
                                };
                                if is_tool {
                                    self.draw_tool_message(ui, message, &theme);
                                } else {
                                    let content_label = egui::Label::new(
                                        egui::RichText::new(message.content.clone())
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
                    self.chat.editing_message_index = None;
                    self.chat.editing_message_content.clear();
                }
            });
        }
    }

    pub(super) fn commit_message_edit(&mut self, index: usize) {
        let new_content = self.chat.editing_message_content.clone();
        // Update chat_display
        if index < self.chat.messages.len() {
            self.chat.messages[index].content = new_content.clone();
        }
        // Update session
        {
            let cl = self.client.lock().unwrap();
            let mut conv = cl.conversation().lock().unwrap();
            if index < conv.len() {
                conv[index].content = new_content;
            }
            drop(conv);
            if let Err(e) = cl.save_session() {
                eprintln!("Failed to save session after edit: {}", e);
            }
        }
        // Clear edit state
        self.chat.editing_message_index = None;
        self.chat.editing_message_content.clear();
    }

    /// Parse a tool message and render it with smart formatting.
    /// Tool messages have the format: "🔧 **tool_name** (call_id)\n```\nresult\n```"
    fn draw_tool_message(&mut self, ui: &mut egui::Ui, message: &ChatMessage, theme: &Theme) {
        let lines: Vec<&str> = message.content.lines().collect();
        if lines.is_empty() {
            return;
        }

        // Render header line (tool name + call_id)
        ui.label(egui::RichText::new(lines[0])
            .color(theme.text_primary)
            .size(11.0));
        ui.add_space(3.0);

        // Extract the raw result text (between ``` fences if present)
        let raw_result = if lines.len() >= 3 && lines[1].starts_with("```") && lines.last().map_or(false, |l| l.starts_with("```")) {
            lines[2..lines.len()-1].join("\n")
        } else {
            message.content.clone()
        };

        // Try to parse as JSON for smart rendering
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw_result) {
            self.draw_tool_json_result(ui, &json, &raw_result, theme);
        } else {
            // Fallback: render as monospace code block
            self.draw_tool_plain_result(ui, &raw_result, theme);
        }
    }

    /// Render a tool result that is valid JSON with smart field extraction.
    fn draw_tool_json_result(&mut self, ui: &mut egui::Ui, json: &serde_json::Value, raw: &str, theme: &Theme) {
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
                                    ui.label(egui::RichText::new(s)
                                        .color(egui::Color32::from_rgb(180, 180, 180))
                                        .monospace());
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
                // File read: show path badge + content preview
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                    let max_preview_len = 2000;
                    if content.len() > max_preview_len {
                        // Truncate long content but show a "show more" option
                        let preview = &content[..max_preview_len];
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(10, 10, 10))
                            .rounding(4.0)
                            .inner_margin(egui::Margin::same(6.0))
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(preview)
                                    .color(egui::Color32::from_rgb(200, 200, 200))
                                    .monospace());
                            });
                        ui.add_space(3.0);
                        let btn = egui::Button::new(format!("Show full content ({} chars)", content.len()))
                            .rounding(4.0);
                        if ui.add(btn).clicked() {
                            // Expand by storing full content in a temporary — for now just show truncated
                            // We use a simple approach: add the full content as a new message-like entry
                            // Actually, let's just show it inline by expanding the frame
                            // Since we can't easily expand, we'll show a scrollable area
                            egui::Frame::none()
                                .fill(egui::Color32::from_rgb(10, 10, 10))
                                .rounding(4.0)
                                .inner_margin(egui::Margin::same(6.0))
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical()
                                        .max_height(300.0)
                                        .show(ui, |ui| {
                                            ui.label(egui::RichText::new(content)
                                                .color(egui::Color32::from_rgb(200, 200, 200))
                                                .monospace());
                                        });
                                });
                        }
                    } else {
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(10, 10, 10))
                            .rounding(4.0)
                            .inner_margin(egui::Margin::same(6.0))
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(content)
                                    .color(egui::Color32::from_rgb(200, 200, 200))
                                    .monospace());
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
                                ui.label(egui::RichText::new(format!("{}:", key))
                                    .color(theme.text_secondary)
                                    .monospace()
                                    .size(11.0));
                                let val_str = Self::json_value_to_string(value);
                                ui.label(egui::RichText::new(val_str)
                                    .color(egui::Color32::from_rgb(200, 200, 200))
                                    .monospace()
                                    .size(11.0));
                            });
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            ui.label(egui::RichText::new(Self::json_value_to_string(item))
                                .color(egui::Color32::from_rgb(200, 200, 200))
                                .monospace()
                                .size(11.0));
                        }
                    }
                    other => {
                        ui.label(egui::RichText::new(Self::json_value_to_string(other))
                            .color(egui::Color32::from_rgb(200, 200, 200))
                            .monospace()
                            .size(11.0));
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
                let items: Vec<String> = arr.iter().map(|v| Self::json_value_to_string(v)).collect();
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
                ui.label(egui::RichText::new(text)
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

    /// Draw the agent pipeline panel with plan status, task progress, and feedback loop info.
    pub(super) fn draw_pipeline_panel(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        // Snapshot pipeline state before the closure
        let plan_id = self.chat.pipeline.plan_id.clone();
        let iteration = self.chat.pipeline.iteration;
        let cancelled = self.chat.pipeline.cancelled;
        let tasks: Vec<_> = self.chat.pipeline.tasks.iter().collect();
        let feedback_state = self.chat.pipeline.feedback_state.clone();
        
        // Panel header with cancel button
        egui::Frame::none()
            .fill(theme.surface_light)
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(8.0))
            .stroke(egui::Stroke::new(1.0, theme.border))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                    // Title
                    let title = format!(
                        "🤖 Agent Pipeline — Plan: {} | Iteration: {}",
                        plan_id, iteration
                    );
                    ui.label(egui::RichText::new(title)
                        .color(theme.text_primary)
                        .size(12.0)
                        .strong());
                    
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Cancel button
                        if !cancelled {
                            let cancel_btn = egui::Button::new("✕ Cancel")
                                .fill(theme.error)
                                .rounding(4.0);
                            if ui.add(cancel_btn).clicked() {
                                if let Some(ref pipeline) = self.agent_pipeline {
                                    pipeline.cancel();
                                }
                                self.chat.pipeline.cancelled = true;
                            }
                        } else {
                            ui.label(egui::RichText::new("⏹ Cancelled")
                                .color(theme.error)
                                .size(11.0));
                        }
                    });
                });
                
                ui.add_space(6.0);
                
                // Task progress list
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                    for task in &tasks {
                        let status_icon = match task.status {
                            crate::ui::state::PipelineTaskStatus::Pending => "⬜",
                            crate::ui::state::PipelineTaskStatus::Running => "🔄",
                            crate::ui::state::PipelineTaskStatus::Completed => "✅",
                            crate::ui::state::PipelineTaskStatus::Failed => "❌",
                        };
                        ui.label(egui::RichText::new(format!(
                            "{} [{}] {}", status_icon, task.id, task.description
                        )).size(11.0));
                        if task.status == crate::ui::state::PipelineTaskStatus::Running {
                            ui.spinner();
                        }
                    }
                });
                
                ui.add_space(4.0);
                
                // Feedback loop status
                if !feedback_state.is_empty() || iteration > 0 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
                        ui.label(egui::RichText::new("🔄 Feedback:")
                            .color(theme.text_secondary)
                            .size(10.0));
                        ui.label(egui::RichText::new(&feedback_state)
                            .color(theme.accent)
                            .size(10.0));
                    });
                }
            });
    }

    /// Draw a compact pipeline progress bar below the chat.
    pub(super) fn draw_pipeline_progress(&self, ui: &mut egui::Ui, theme: &Theme) {
        let pipeline = &self.chat.pipeline;
        if pipeline.tasks.is_empty() {
            return;
        }
        let total = pipeline.tasks.len();
        let completed = pipeline.tasks.iter()
            .filter(|t| t.status == crate::ui::state::PipelineTaskStatus::Completed)
            .count();
        let failed = pipeline.tasks.iter()
            .filter(|t| t.status == crate::ui::state::PipelineTaskStatus::Failed)
            .count();
        let running = pipeline.tasks.iter()
            .filter(|t| t.status == crate::ui::state::PipelineTaskStatus::Running)
            .count();
        let _pending = total - completed - failed - running;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
            ui.label(egui::RichText::new("Progress:")
                .color(theme.text_secondary)
                .size(10.0));
            ui.add(egui::ProgressBar::new(completed as f32 / total.max(1) as f32));
            ui.label(egui::RichText::new(format!(
                "{}/{} tasks", completed, total
            )).color(theme.text_secondary).size(10.0));
            if running > 0 {
                ui.spinner();
            }
            if failed > 0 {
                ui.label(egui::RichText::new(format!("❌{}", failed))
                    .color(theme.error).size(10.0));
            }
            if pipeline.cancelled {
                ui.label(egui::RichText::new(" ⏹ Cancelled")
                    .color(theme.error).size(10.0));
            }
        });
    }

    pub(super) fn delete_message(&mut self, index: usize) {
        // Remove from chat_display
        if index < self.chat.messages.len() {
            self.chat.messages.remove(index);
        }
        // Remove from session
        {
            let cl = self.client.lock().unwrap();
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
