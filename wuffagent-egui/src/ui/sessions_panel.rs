use eframe::egui;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use crate::config::Config;
use crate::sessions;
use super::theme::Theme;
use super::sessions_actions::PanelAction;
use super::sessions_utils::{relative_time, truncate};

#[derive(Clone, Debug)]
pub struct SessionsPanel {
    sessions: Vec<crate::sessions::Session>,
    selected_id: Option<String>,
    sessions_dir: PathBuf,
    config: Config,
    renaming: Option<String>,
    rename_input: String,
    creating: bool,
    new_name: String,
    /// If Some, holds the pending delete confirmation dialog state: (id, name, last_message).
    pending_delete: Option<(String, String, String)>,
    /// Notification message shown briefly after an operation.
    notification: Option<(String, bool)>,
    /// Time when the notification was shown (for auto-dismiss).
    notification_start: f64,
    /// Path for export (set when user clicks Export).
    export_path: String,
    /// Path for import (set when user clicks Import).
    import_path: String,
}

impl SessionsPanel {
    pub fn new(config: &Arc<Mutex<Config>>) -> Self {
        let cfg = config.lock().unwrap();
        let dir = cfg.sessions_dir().clone();
        let sessions = sessions::list_sessions(&dir);
        let selected_id = sessions.first().map(|s| s.id.clone());
        let config_clone = cfg.clone();
        drop(cfg);
        Self {
            sessions,
            selected_id,
            sessions_dir: dir,
            config: config_clone,
            renaming: None,
            rename_input: String::new(),
            creating: false,
            new_name: String::new(),
            pending_delete: None,
            notification: None,
            notification_start: 0.0,
            export_path: String::new(),
            import_path: String::new(),
        }
    }

    pub fn refresh(&mut self) {
        self.sessions = sessions::list_sessions(&self.sessions_dir);
    }

    /// Clear the selected session (used after deletion).
    pub fn clear_session(&mut self) {
        self.selected_id = None;
    }

    /// Select a session by id without clearing it first.
    pub fn select_session(&mut self, id: &str) {
        self.selected_id = Some(id.to_string());
    }

    pub(super) fn sessions_dir(&self) -> &PathBuf {
        &self.sessions_dir
    }

    pub(super) fn selected_id(&self) -> &Option<String> {
        &self.selected_id
    }

    pub(super) fn selected_id_mut(&mut self) -> &mut Option<String> {
        &mut self.selected_id
    }

    pub(super) fn config(&self) -> &Config {
        &self.config
    }

    pub(super) fn export_path(&self) -> &str {
        &self.export_path
    }

    pub(super) fn import_path(&self) -> &str {
        &self.import_path
    }

    /// Show a brief notification message.
    pub fn show_notification(&mut self, msg: &str, success: bool) {
        self.notification = Some((msg.to_string(), success));
        self.notification_start = chrono::Utc::now().timestamp_millis() as f64;
    }

    /// Check if the current notification has expired (after ~3 seconds).
    pub fn update_notification(&mut self, ctx: &egui::Context) {
        if self.notification.is_some() {
            let elapsed = (chrono::Utc::now().timestamp_millis() as f64 - self.notification_start) / 1000.0;
            if elapsed > 3.0 {
                self.notification = None;
                ctx.request_repaint();
            }
        }
    }

    /// Check if a session is currently generating (has active task).
    pub fn is_session_generating(
        &self,
        session_store: &std::collections::HashMap<String, crate::sessions::SessionRuntime>,
        session_id: &str,
    ) -> bool {
        session_store.get(session_id).map(|r| r.is_generating()).unwrap_or(false)
    }

    /// Render the session button with metadata: name, message count,
    /// last message preview, and relative timestamp.
    fn draw_session_item(
        ui: &mut egui::Ui,
        session: &crate::sessions::Session,
        is_selected: bool,
        is_renaming: bool,
        is_generating: bool,
    ) -> egui::Response {
        let count = session.messages.len();
        let count_text = if count == 1 {
            "1 message".to_string()
        } else {
            format!("{} messages", count)
        };

        let last_preview: String = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user" || m.role == "assistant")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let preview_truncated = truncate(&last_preview, 50);

        let timestamp_text = relative_time(&session.updated_at);

        let label = if is_generating {
            format!(
                "▶ {}\n{}  •  {}\n{}",
                session.name,
                count_text,
                preview_truncated,
                timestamp_text
            )
        } else {
            format!(
                "{}\n{}  •  {}\n{}",
                session.name,
                count_text,
                preview_truncated,
                timestamp_text
            )
        };

        let display_label = if is_renaming {
            format!("{}\n🔄 rename", label)
        } else {
            label
        };

        let response = ui.add(egui::Button::new(display_label)
            .fill(if is_selected || is_renaming {
                egui::Color32::from_rgba_premultiplied(59, 130, 246, 38) // #3B82F6 @ 15%
            } else {
                egui::Color32::TRANSPARENT
            })
            .corner_radius(4)
            .sense(egui::Sense::click()));

        response
    }

    /// Draw the sessions sidebar.
    ///
    /// `theme` and `session_store` are passed as separate immutable references
    /// (disjoint from `self`, which is the panel) so the caller can borrow the
    /// panel mutably while still reading app state. Returns the id of any
    /// session row that was clicked, plus any pending action to apply.
    pub fn draw(
        &mut self,
        theme: &str,
        session_store: &std::collections::HashMap<String, crate::sessions::SessionRuntime>,
        ui: &mut egui::Ui,
    ) -> (Option<String>, Option<PanelAction>) {
        let ctx = ui.ctx();
        let mut selected_id: Option<String> = None;
        let mut action: Option<PanelAction> = None;
        // Show the delete confirmation dialog if pending
        if let Some((id, name, last_message)) = self.pending_delete.clone() {
            egui::Window::new("Delete Session")
                .collapsible(false)
                .resizable(false)
                .movable(true)
                .default_pos([300.0, 200.0])
                .show(ctx, |ui| {
                    ui.weak("Are you sure you want to delete this session?");
                    ui.separator();
                    ui.label(format!("Session: {}", name));
                    if !last_message.is_empty() {
                        let truncated = truncate(&last_message, 50);
                        ui.weak(format!("Last message: {}", truncated));
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            action = Some(PanelAction::Delete(id.clone()));
                            self.pending_delete = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending_delete = None;
                        }
                    });
                });
        }

        egui::Panel::left("sessions_panel")
            .default_size(220.0)
            .min_size(150.0)
            .max_size(320.0)
            .show(ui, |ui| {
                let theme = Theme::from_name(theme);
                ui.visuals_mut().panel_fill = theme.panel_bg;
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 8.0);
                // Session heading with accent color
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(egui::RichText::new("SESSIONS")
                    .color(theme.text_secondary)
                    .size(11.0)
                    .strong());
                ui.separator();

                // New session button
                let new_btn = egui::Button::new("+ New")
                    .fill(theme.primary)
                    .corner_radius(4);
                if ui.add(new_btn).clicked() {
                    self.creating = true;
                    self.new_name = String::new();
                }

                // Delete button
                let delete_btn = egui::Button::new("Delete")
                    .fill(theme.surface_light)
                    .corner_radius(4);
                if ui.add(delete_btn).clicked() {
                    let session_id = self.selected_id.clone();
                    let session = self.sessions.iter().find(|s| Some(&s.id) == session_id.as_ref());
                    if let Some(session) = session {
                        let last_msg: String = session
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.role == "user" || m.role == "assistant")
                            .map(|m| m.content.clone())
                            .unwrap_or_default();
                        self.pending_delete = Some((
                            session.id.clone(),
                            session.name.clone(),
                            last_msg,
                        ));
                    } else {
                        self.show_notification("No session selected", false);
                    }
                }

                ui.add_space(8.0);

                // Show notification as a toast
                if let Some((msg, success)) = &self.notification {
                    let color = if *success {
                        theme.success
                    } else {
                        theme.error
                    };
                    ui.colored_label(color, msg.clone());
                }

                // Handle keyboard shortcuts globally
                ui.ctx().input(|i| {
                    // F2 to start renaming selected session
                    if i.key_pressed(egui::Key::F2) && self.renaming.is_none() {
                        if let Some(ref id) = self.selected_id {
                            if let Some(session) = self.sessions.iter().find(|s| &s.id == id) {
                                self.renaming = Some(id.clone());
                                self.rename_input = session.name.clone();
                            }
                        }
                    }
                    // Enter to confirm rename
                    if i.key_pressed(egui::Key::Enter)
                        && self.renaming.is_some() {
                            let new_name = self.rename_input.trim().to_string();
                            if !new_name.is_empty() {
                                action = Some(PanelAction::Rename {
                                    id: self.renaming.clone().unwrap(),
                                    new_name: new_name.clone(),
                                });
                            }
                            self.renaming = None;
                            self.rename_input.clear();
                        }
                    // Escape to cancel rename
                    if i.key_pressed(egui::Key::Escape) && self.renaming.is_some() {
                        self.renaming = None;
                        self.rename_input.clear();
                    }
                });

                // Session list
                ui.add_space(4.0);

                // Collect actions to avoid borrowing self inside the loop
                for session in &self.sessions {
                    let is_renaming = Some(&session.id) == self.renaming.as_ref();
                    let is_generating = self.is_session_generating(session_store, &session.id);
                    
                    if is_renaming {
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.rename_input).request_focus();
                            if ui.button("✓").clicked() {
                                action = Some(PanelAction::Rename {
                                    id: session.id.clone(),
                                    new_name: self.rename_input.clone(),
                                });
                                self.renaming = None;
                                self.rename_input.clear();
                            }
                            if ui.button("✗").clicked() {
                                self.renaming = None;
                                self.rename_input.clear();
                            }
                        });
                        continue;
                    }

                    let is_selected = Some(&session.id) == self.selected_id.as_ref();
                    let response = Self::draw_session_item(ui, session, is_selected, is_renaming, is_generating);

                    if response.clicked() {
                        selected_id = Some(session.id.clone());
                        self.selected_id = Some(session.id.clone());
                        // Update config so the selected session is loaded on next app start
                        self.config.session_id = Some(session.id.clone());
                        if let Err(e) = self.config.save() {
                            eprintln!("Failed to save config after selecting session: {}", e);
                        }
                    }
                    if response.double_clicked() {
                        self.renaming = Some(session.id.clone());
                        self.rename_input = session.name.clone();
                    }
                }

                // Export/Import section
                ui.add_space(12.0);
                ui.separator();
                ui.label(egui::RichText::new("EXPORT / IMPORT")
                    .color(theme.text_secondary)
                    .size(11.0)
                    .strong());
                ui.horizontal(|ui| {
                    let export_btn = egui::Button::new("Export")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui.add(export_btn).clicked()
                        && self.selected_id.is_some() {
                            action = Some(PanelAction::Export {
                                session_id: self.selected_id.clone().unwrap(),
                            });
                        }
                    let import_btn = egui::Button::new("Import")
                        .fill(theme.surface_light)
                        .corner_radius(4);
                    if ui.add(import_btn).clicked() {
                        action = Some(PanelAction::Import);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Export path:").size(11.0).color(theme.text_secondary));
                    ui.text_edit_singleline(&mut self.export_path);
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Import path:").size(11.0).color(theme.text_secondary));
                    ui.text_edit_singleline(&mut self.import_path);
                });

                if self.creating {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Name:").size(11.0).color(theme.text_secondary));
                        let resp = ui.text_edit_singleline(&mut self.new_name);
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            && !self.new_name.trim().is_empty() {
                                action = Some(PanelAction::Create(self.new_name.clone()));
                                self.new_name.clear();
                                self.creating = false;
                            }
                        if ui.button("Create").clicked() && !self.new_name.trim().is_empty() {
                            action = Some(PanelAction::Create(self.new_name.clone()));
                            self.new_name.clear();
                            self.creating = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.creating = false;
                            self.new_name.clear();
                        }
                    });
                }
            });

        // Return the clicked session id (if any) and the pending action for the
        // caller (layout.rs) to apply — applying here would conflict with the
        // mutable borrow of `self`.
        (selected_id, action)
    }
}
