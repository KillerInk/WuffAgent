use eframe::egui;

use super::window::{ChatApp, ChatMessage};

impl ChatApp {
    pub(super) fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        // Show pending error as inline warning
        if let Some(ref err) = self.pending_error {
            let err_clone = err.clone();
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", err_clone));
                if ui.button("Dismiss").clicked() {
                    self.pending_error = None;
                }
            });
            ui.separator();
        }

        // Clone messages to avoid borrow checker issues
        let messages: Vec<ChatMessage> = self.chat_display.clone();

        // Use a string salt for consistent ID matching between id_salt and State::load/store
        // id_salt("chat_scroll") produces the ID ui.make_id("chat_scroll") internally
        let scroll_salt = "chat_scroll";
        // id_salt internally uses ui.id().with(salt) - match it exactly for State::load/store
        let scroll_id = ui.id().with(scroll_salt);

        // Load the persisted scroll state using the SAME ID the ScrollArea will use
        let prev_state = egui::containers::scroll_area::State::load(ui.ctx(), scroll_id);
        let prev_offset_y = prev_state.as_ref().map(|s| s.offset.y).unwrap_or(0.0);

        // If auto-scrolling and was at bottom, pre-store a large offset so egui clamps to bottom
        let should_scroll_to_bottom = self.auto_scroll && self.auto_scroll_at_bottom;
        if should_scroll_to_bottom {
            let mut s = prev_state.unwrap_or_default();
            s.offset.y = f32::MAX;
            s.store(ui.ctx(), scroll_id);
        }

        // Build scroll area with id_salt matching our salt
        let scroll_area = egui::ScrollArea::vertical()
            .id_salt(scroll_salt)
            .auto_shrink([false, true])
            .max_width(ui.available_width())
            .stick_to_bottom(true);

        let output = scroll_area.show_viewport(ui, |ui, _viewport| {
            ui.vertical(|ui| {
                for (i, msg) in messages.iter().enumerate() {
                    self.draw_message(ui, msg, i);
                }

                // Show current streaming response
                let streaming_ts = chrono::Local::now().format("%H:%M:%S").to_string();
                if self.is_generating && !self.current_response.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(egui::RichText::new(&streaming_ts)
                            .color(egui::Color32::GRAY)
                            .size(11.0));
                        ui.separator();
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "AI:");
                        ui.separator();
                        ui.add(egui::Label::new(
                            egui::RichText::new(&self.current_response)
                                .color(egui::Color32::from_rgb(150, 200, 150))
                        ));
                        ui.spinner();
                    });
                } else if self.is_generating && self.current_response.is_empty() {
                    // Show spinner while waiting for first chunk
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(egui::RichText::new(&streaming_ts)
                            .color(egui::Color32::GRAY)
                            .size(11.0));
                        ui.separator();
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "AI:");
                        ui.spinner();
                    });
                }
            });
        });

        // Read scroll state from output (this is the actual state used by the scroll area)
        let state = output.state;
        let content_height = output.content_size.y;
        let inner_height = output.inner_rect.height();
        let max_offset = (content_height - inner_height).max(0.0);
        let at_bottom = max_offset <= 1.0 || state.offset.y >= max_offset - 1.0;

        // Detect if user manually scrolled up (offset decreased from previous frame)
        if self.auto_scroll_at_bottom && prev_offset_y > 0.0 && state.offset.y < prev_offset_y - 1.0 {
            self.auto_scroll = false;
        }
        self.auto_scroll_at_bottom = at_bottom;

        // Persist the state after each frame
        state.store(ui.ctx(), scroll_id);

        // Show scroll-to-bottom button when not at bottom
        if !at_bottom {
            let button_size = egui::vec2(28.0, 28.0);
            let button_pos = ui.max_rect().right_top() - egui::vec2(button_size.x + 8.0, 8.0);
            let button_rect = egui::Rect::from_min_size(button_pos, button_size);
            let ctx = ui.ctx().clone();
            let bid = scroll_id;
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(button_rect), |ui| {
                ui.set_max_size(button_size);
                ui.set_min_size(button_size);
                if ui.button("↓").clicked() {
                    // Pre-store large offset so next frame scrolls to bottom (egui clamps to max)
                    let mut new_state = state.clone();
                    new_state.offset.y = f32::MAX;
                    new_state.store(&ctx, bid);
                    self.auto_scroll = true;
                    self.auto_scroll_at_bottom = true;
                    ctx.request_repaint();
                }
            });
        }
    }

    pub(super) fn draw_message(
        &mut self,
        ui: &mut egui::Ui,
        message: &ChatMessage,
        index: usize,
    ) {
        let is_user = message.role == "user";
        let is_editing = self.editing_message_index == Some(index);
        
        // Use a scoped UI to constrain the width of the content
        let max_content_width = (ui.available_width() - 80.0).max(100.0);
        
        // Handle right-click context menu for edit/delete
        let response = ui.interact(ui.max_rect(), ui.id().with(index), egui::Sense::click());
        if !is_editing && response.secondary_clicked() {
            response.context_menu(|ui| {
                ui.set_min_width(120.0);
                if ui.button("Edit").clicked() {
                    self.editing_message_index = Some(index);
                    self.editing_message_content = message.content.clone();
                }
                if ui.button("Delete").clicked() {
                    self.delete_message(index);
                }
            });
        }
        
        // Align based on role: user left, AI right
        if is_user {
            // User messages: left-aligned
            ui.horizontal_wrapped(|ui| {
                ui.scope(|ui| {
                    ui.set_max_width(max_content_width);
                    
                    // Timestamp (small, gray)
                    ui.label(egui::RichText::new(&message.timestamp)
                        .color(egui::Color32::GRAY)
                        .size(11.0));
                    ui.separator();
                    
                    // Role (colored)
                    ui.colored_label(egui::Color32::LIGHT_BLUE, "You:");
                    ui.separator();
                    
                    // Content with wrapping
                    ui.vertical(|ui| {
                        if is_editing {
                            // Edit mode: show text input
                            ui.text_edit_multiline(&mut self.editing_message_content);
                        } else {
                            ui.label(egui::RichText::new(&message.content));
                        }
                        
                        // Display image if present
                        if let Some(ref img_data) = message.image {
                            if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, img_data) {
                                let img = egui::Image::from_bytes("image", decoded);
                                let max_img_width = (ui.available_width() - 10.0).max(50.0);
                                ui.add(img.max_size(egui::Vec2::new(max_img_width, 300.0)));
                            }
                        }
                    });
                });
            });
        } else {
            // AI messages: right-aligned
            ui.horizontal_wrapped(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.scope(|ui| {
                        ui.set_max_width(max_content_width);
                        
                        // Content with wrapping
                        ui.vertical(|ui| {
                            if is_editing {
                                // Edit mode: show text input
                                ui.text_edit_multiline(&mut self.editing_message_content);
                            } else {
                                // Display image if present (above text for AI)
                                if let Some(ref img_data) = message.image {
                                    if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, img_data) {
                                        let img = egui::Image::from_bytes("image", decoded);
                                        let max_img_width = (ui.available_width() - 10.0).max(50.0);
                                        ui.add(img.max_size(egui::Vec2::new(max_img_width, 300.0)));
                                    }
                                }
                                ui.label(egui::RichText::new(&message.content));
                            }
                        });
                        
                        ui.separator();
                        
                        // Role (colored)
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "AI:");
                        ui.separator();
                        
                        // Timestamp (small, gray)
                        ui.label(egui::RichText::new(&message.timestamp)
                            .color(egui::Color32::GRAY)
                            .size(11.0));
                    });
                });
            });
        }
        
        // Handle keyboard shortcuts when editing
        if is_editing {
            ui.ctx().input(|i| {
                if i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl {
                    // Ctrl+Enter to commit edit
                    self.commit_message_edit(index);
                }
                if i.key_pressed(egui::Key::Escape) {
                    // Escape to cancel edit
                    self.editing_message_index = None;
                    self.editing_message_content.clear();
                }
            });
        }
    }

    pub(super) fn commit_message_edit(&mut self, index: usize) {
        let new_content = self.editing_message_content.clone();
        // Update chat_display
        if index < self.chat_display.len() {
            self.chat_display[index].content = new_content.clone();
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
        self.editing_message_index = None;
        self.editing_message_content.clear();
    }

    pub(super) fn delete_message(&mut self, index: usize) {
        // Remove from chat_display
        if index < self.chat_display.len() {
            self.chat_display.remove(index);
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
