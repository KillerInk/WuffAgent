use eframe::egui;

use super::state::ChatApp;
use super::theme::Theme;

impl ChatApp {
    pub(super) fn setup_ui(&mut self, ctx: &egui::Context) {
        // Session sidebar — draw before other panels so it sits on the left
        let (switched_id, clear_client_session) = {
            if let Some(ref mut panel) = self.sessions.sessions_panel {
                panel.draw(ctx)
            } else {
                (None, false)
            }
        };

        if let Some(ref mut panel) = self.sessions.sessions_panel {
            panel.update_notification(ctx);
        }

        // Switch session if needed (handles New button and history selection)
        if let Some(id) = switched_id {
            self.switch_session(&id);
        }

        // Clear client session if a session was deleted (explicit flag from panel).
        // Also clear the chat display so the deleted conversation isn't re-saved.
        if clear_client_session {
            self.client.lock().unwrap().clear_session();
            self.chat.messages.clear();
        }

        egui::TopBottomPanel::top("menu_bar").resizable(false).show(ctx, |ui| {
            ui.set_min_height(32.0);
            ui.set_max_height(36.0);

            let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
            ui.visuals_mut().panel_fill = theme.background;

            ui.horizontal(|ui| {
                // App title with accent color
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.visuals_mut().override_text_color = Some(theme.primary);
                ui.heading("WuffAgent");
                ui.visuals_mut().override_text_color = None;

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Theme toggle button
                    let theme_btn = egui::Button::new("◐")
                        .fill(theme.surface_light)
                        .rounding(4.0);
                    if ui.add(theme_btn).clicked() {
                        self.toggle_theme(ctx);
                    }

                    // Agent config button
                    let agent_btn = egui::Button::new("🤖")
                        .fill(theme.surface_light)
                        .rounding(4.0);
                    if ui.add(agent_btn).clicked() {
                        self.show_agent_config = true;
                    }

                    // Settings button
                    let settings_btn = egui::Button::new("⚙")
                        .fill(theme.surface_light)
                        .rounding(4.0);
                    if ui.add(settings_btn).clicked() {
                        self.show_settings = true;
                    }
                });
            });
        });

        // Bottom panels stack upward, so bottom_bar must be declared first to be at the bottom
        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
            ui.visuals_mut().panel_fill = theme.surface;
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
            self.draw_status_bar(ui);
            ui.separator();
            self.draw_bottom_bar(ui);
        });

        // Input area is its own bottom panel, anchored above the status bar
        egui::TopBottomPanel::bottom("input_panel")
            .default_height(50.0)
            .resizable(false)
            .show(ctx, |ui| {
                let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
                ui.visuals_mut().panel_fill = theme.surface;
                self.draw_input_area(ui);
            });

        // Chat area fills all remaining space between top bar and input panel
        egui::CentralPanel::default().show(ctx, |ui| {
            let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
            ui.visuals_mut().panel_fill = theme.background;
            self.draw_chat_area(ui);
        });

        // Agent chain side panel — always shown so the user can see agent activity
        egui::SidePanel::right("agent_chain_panel")
            .default_width(280.0)
            .resizable(true)
            .show(ctx, |ui| {
                self.draw_agent_chain_panel(ui);
            });
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        let cfg = self.config.lock().unwrap();
        let current_theme = cfg.theme.clone();
        drop(cfg);

        let new_theme = if current_theme == "dark" {
            "light".to_string()
        } else {
            "dark".to_string()
        };

        self.config.lock().unwrap().theme = new_theme.clone();
        if let Err(e) = self.config.lock().unwrap().save() {
            eprintln!("Failed to save theme: {}", e);
        }

        // Apply custom theme colors
        let theme = Theme::from_name(&new_theme);
        theme.apply(ctx);
    }
}
