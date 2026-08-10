use eframe::egui;

use super::state::ChatApp;
use super::window::ChatMessage;
use super::theme::Theme;

impl ChatApp {
    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
        
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

        // Use egui's built-in scroll area with id_salt
        // egui automatically persists scroll state via stick_to_bottom
        egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, true])
            .stick_to_bottom(self.chat.auto_scroll)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    for (i, msg) in messages.iter().enumerate() {
                        self.draw_message(ui, msg, i, &theme);
                    }

                    // Show current streaming response
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
                        // Show spinner while waiting for first chunk
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
            });

        // Show scroll-to-bottom button when not at bottom
        // Use egui's built-in scroll state to detect position
        let scroll_id = ui.id().with("chat_scroll");
        let at_bottom = self.is_at_bottom(ui, scroll_id);

        if !at_bottom {
            let button_size = egui::vec2(32.0, 32.0);
            let button_pos = ui.max_rect().right_top() - egui::vec2(button_size.x + 12.0, 12.0);
            let button_rect = egui::Rect::from_min_size(button_pos, button_size);
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(button_rect), |ui| {
                ui.set_max_size(button_size);
                ui.set_min_size(button_size);
                let scroll_btn = egui::Button::new("↓")
                    .fill(theme.primary)
                    .rounding(16.0);
                if ui.add(scroll_btn).clicked() {
                    // Enable auto-scroll; stick_to_bottom will handle the rest
                    self.chat.auto_scroll = true;
                }
            });
        }
    }

    fn is_at_bottom(&self, ui: &egui::Ui, scroll_id: egui::Id) -> bool {
        if let Some(state) = egui::containers::scroll_area::State::load(ui.ctx(), scroll_id) {
            let content_height = state.offset.y + ui.max_rect().height();
            state.offset.y >= content_height - 1.0
        } else {
            true // No scroll state yet, assume at bottom
        }
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
                        let bubble_inner_padding = egui::vec2(8.0, 6.0);
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
                                // User messages: white text on dark blue bubble for contrast
                                // AI messages: dark text on light bubble for contrast
                                let text_color = if is_user {
                                    egui::Color32::WHITE
                                } else {
                                    theme.text_primary
                                };
                                let content_label = egui::Label::new(
                                    egui::RichText::new(&message.content)
                                        .color(text_color)
                                ).wrap();
                                ui.add(content_label);
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
