use eframe::egui;

use super::state::ChatApp;
use super::theme::Theme;

impl ChatApp {
    pub(super) fn setup_ui(&mut self, ui: &mut egui::Ui) {
        // Session sidebar — draw before other panels so it sits on the left.
        // Pass disjoint immutable borrows (theme string + session_store) so the
        // panel (a field of `self`) can be borrowed mutably while we read app state.
        let theme = self.config.theme.clone();
        let (switched_id, pending_action) = {
            if let Some(ref mut panel) = self.sessions_panel {
                panel.draw(&theme, &self.session_store, ui)
            } else {
                (None, None)
            }
        };

        if let Some(ref mut panel) = self.sessions_panel {
            panel.update_notification(ui.ctx());
        }

        // Apply any pending panel action (create/delete/rename/export/import).
        // This mutates session_store via apply_actions.
        if let Some(action) = pending_action {
            self.apply_sessions_action(action);
        }

        // Switch session if needed (handles New button and history selection)
        if let Some(id) = switched_id {
            self.switch_session(&id);
        }

        egui::Panel::top("menu_bar").resizable(false).show(ui, |ui| {
            ui.set_min_height(32.0);
            ui.set_max_height(36.0);

            let theme = Theme::from_name(&self.config.theme);
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
                        .corner_radius(4);
                    if ui
                        .add(theme_btn)
                        .on_hover_text("Toggle light/dark theme")
                        .clicked()
                    {
                        self.toggle_theme(ui.ctx());
                    }

                    // Memory panel button
                    let memory_btn = egui::Button::new("🧠")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui
                        .add(memory_btn)
                        .on_hover_text("Memory panel")
                        .clicked()
                    {
                        self.memory_panel.show_panel = true;
                    }

                    // Token usage panel button
                    let usage_btn = egui::Button::new("📊")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui
                        .add(usage_btn)
                        .on_hover_text("Token usage")
                        .clicked()
                    {
                        self.usage_panel.show_panel = true;
                    }

                    // MCP servers button
                    let mcp_btn = egui::Button::new("🔌")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui
                        .add(mcp_btn)
                        .on_hover_text("MCP servers (external tools)")
                        .clicked()
                    {
                        self.mcp_panel.show_panel = true;
                    }

                    // Improvements panel button
                    let improvements_btn = egui::Button::new("✨")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui
                        .add(improvements_btn)
                        .on_hover_text("Agent improvements (pending suggestions)")
                        .clicked()
                    {
                        self.improvements_panel.show_panel = true;
                    }

                    // Agent config button
                    let agent_btn = egui::Button::new("🤖")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui
                        .add(agent_btn)
                        .on_hover_text("Agent configuration")
                        .clicked()
                    {
                        self.show_agent_config = true;
                    }

                    // Settings button
                    let settings_btn = egui::Button::new("⚙")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui
                        .add(settings_btn)
                        .on_hover_text("Settings")
                        .clicked()
                    {
                        self.show_settings = true;
                    }
                });
            });
        });

        // Bottom panels stack upward, so bottom_bar must be declared first to be at the bottom
        egui::Panel::bottom("bottom_bar").show(ui, |ui| {
            let theme = Theme::from_name(&self.config.theme);
            ui.visuals_mut().panel_fill = theme.surface;
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
            self.draw_status_bar(ui);
            ui.separator();
            self.draw_bottom_bar(ui);
        });

        // Input area is its own bottom panel, anchored above the status bar
        egui::Panel::bottom("input_panel")
            .default_size(110.0)
            .min_size(70.0)
            .show(ui, |ui| {
                let theme = Theme::from_name(&self.config.theme);
                ui.visuals_mut().panel_fill = theme.surface;
                self.draw_input_area(ui);
            });

        // Chat area fills all remaining space
        egui::CentralPanel::default().show(ui, |ui| {
            let theme = Theme::from_name(&self.config.theme);
            ui.visuals_mut().panel_fill = theme.background;
            self.draw_chat_area(ui);
        });

        // Draw improvements panel on top
        let ctx = ui.ctx();
        self.draw_improvements_panel(ctx);

        // Draw memory panel on top (disjoint field borrows).
        self.draw_memory_panel(ctx);

        // Draw token-usage panel on top.
        self.usage_panel.draw(ctx, &Theme::from_name(&self.config.theme));

        // Draw MCP panel on top (disjoint field borrows; `&mut self.config`
        // so the panel can persist server changes).
        self.draw_mcp_panel(ctx);
    }

    fn draw_memory_panel(&mut self, ctx: &egui::Context) {
        // Disjoint field borrows: the panel (mutable) + manager, runtime, config
        // (immutable) are separate struct fields, so they can coexist.
        let updated = self
            .memory_panel
            .draw(ctx, &self.memory_manager, self.memory_runtime.as_ref(), &self.config);
        if let Some(mconfig) = updated {
            // I4: persist memory settings — before this they were runtime-only
            // (set_config) and reverted on restart.
            self.config.memory_config = mconfig;
            if let Err(e) = self.save_config() {
                eprintln!("Failed to save memory settings: {}", e);
            }
        }
    }

    fn draw_mcp_panel(&mut self, ctx: &egui::Context) {
        // Disjoint field borrows: the panel (mutable) + manager (immutable)
        // + config (mutable) are separate struct fields, so they can coexist.
        self.mcp_panel.draw(ctx, &self.mcp_manager, &mut self.config);
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        let new_theme: &str = if self.config.theme == "dark" { "light" } else { "dark" };
        self.config.theme = new_theme.to_string();
        if let Err(e) = self.save_config() {
            eprintln!("Failed to save theme: {}", e);
        }

        // Apply custom theme colors
        Theme::from_name(new_theme).apply(ctx);
    }
}
