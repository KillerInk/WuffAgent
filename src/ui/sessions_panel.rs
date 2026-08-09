use eframe::egui;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use crate::config::Config;
use crate::sessions;

pub struct SessionsPanel {
    sessions: Vec<crate::sessions::Session>,
    selected_id: Option<String>,
    sessions_dir: PathBuf,
    renaming: Option<String>,
    rename_input: String,
    creating: bool,
    new_name: String,
    pub(super) clear_action: bool,
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

#[derive(Debug)]
enum PanelAction {
    Rename { id: String, new_name: String },
    Create(String),
    Delete(String),
    Export { session_id: String },
    Import,
}

/// Format a chrono DateTime<Utc> to a human-readable relative time string
/// like "just now", "5 min ago", "2 hours ago", "3 days ago".
fn relative_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*dt);
    let seconds = duration.num_seconds().abs();

    if seconds < 10 {
        "just now".to_string()
    } else if seconds < 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        if minutes < 2 {
            "1 min ago".to_string()
        } else {
            format!("{} min ago", minutes)
        }
    } else if seconds < 86400 {
        let hours = seconds / 3600;
        if hours < 2 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if seconds < 172800 {
        "yesterday".to_string()
    } else {
        let days = seconds / 86400;
        if days < 30 {
            format!("{} days ago", days)
        } else if days < 365 {
            let months = days / 30;
            if months < 2 {
                "1 month ago".to_string()
            } else {
                format!("{} months ago", months)
            }
        } else {
            let years = days / 365;
            if years < 2 {
                "1 year ago".to_string()
            } else {
                format!("{} years ago", years)
            }
        }
    }
}

/// Truncate a string to at most `max_chars` characters, adding "..." if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max_chars).collect::<String>())
    }
}

impl SessionsPanel {
    pub fn new(config: &Arc<Mutex<Config>>) -> Self {
        let cfg = config.lock().unwrap();
        let dir = cfg.sessions_dir().clone();
        drop(cfg);
        let sessions = sessions::list_sessions(&dir);
        Self {
            sessions,
            selected_id: None,
            sessions_dir: dir,
            renaming: None,
            rename_input: String::new(),
            creating: false,
            new_name: String::new(),
            clear_action: false,
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

    /// Render the session button with metadata: name, message count,
    /// last message preview, and relative timestamp.
    fn draw_session_button(
        ui: &mut egui::Ui,
        session: &crate::sessions::Session,
        is_selected: bool,
        is_renaming: bool,
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

        // Build the button label with multiple lines.
        let label = format!(
            "{}\n{}  •  {}\n{}",
            session.name,
            count_text,
            preview_truncated,
            timestamp_text
        );

        // When renaming, show a subtle rename indicator in the label
        let display_label = if is_renaming {
            format!("{}\n🔄 rename", label)
        } else {
            label
        };

        ui.add(egui::Button::new(display_label).fill(if is_selected || is_renaming {
            egui::Color32::from_rgb(50, 50, 100)
        } else {
            egui::Color32::TRANSPARENT
        }))
    }

    pub fn draw(&mut self, ctx: &egui::Context) -> Option<String> {
        let mut selected_id: Option<String> = None;
        let mut action: Option<PanelAction> = None;

        // Show the confirmation dialog if pending
        if let Some((id, name, last_message)) = self.pending_delete.take() {
            let mut should_delete = None;
            egui::Window::new("Delete Session")
                .collapsible(false)
                .resizable(false)
                .movable(true)
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
                            should_delete = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            should_delete = Some(false);
                        }
                    });
                });
            match should_delete {
                Some(true) => {
                    action = Some(PanelAction::Delete(id));
                }
                Some(false) | None => {
                    // Cancel or window closed — do not delete
                    self.pending_delete = None;
                }
            }
        }

        egui::SidePanel::left("sessions_panel")
            .default_width(200.0)
            .min_width(150.0)
            .max_width(300.0)
            .show(ctx, |ui| {
                ui.heading("Sessions");
                ui.separator();

                if ui.button("+ New").clicked() {
                    self.creating = true;
                    self.new_name = String::new();
                }
                if ui.button("Clear").clicked() {
                    self.clear_action = true;
                }

                ui.separator();

                // Show notification as a toast
                if let Some((msg, success)) = &self.notification {
                    let color = if *success {
                        egui::Color32::from_rgb(0, 180, 0)
                    } else {
                        egui::Color32::from_rgb(220, 50, 50)
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
                    if i.key_pressed(egui::Key::Enter) {
                        if self.renaming.is_some() {
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
                    }
                    // Escape to cancel rename
                    if i.key_pressed(egui::Key::Escape) && self.renaming.is_some() {
                        self.renaming = None;
                        self.rename_input.clear();
                    }
                });

                // Collect actions to avoid borrowing self inside the loop
                for session in &self.sessions {
                    let is_renaming = Some(&session.id) == self.renaming.as_ref();
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
                    let response = Self::draw_session_button(ui, session, is_selected, is_renaming);
                    if response.clicked() {
                        selected_id = Some(session.id.clone());
                        self.selected_id = Some(session.id.clone());
                    }
                    if response.double_clicked() {
                        self.renaming = Some(session.id.clone());
                        self.rename_input = session.name.clone();
                    }
                    // Right-click to open context menu with export option
                    if response.secondary_clicked() {
                        response.context_menu(|ui| {
                            ui.set_min_width(120.0);
                            if ui.button("Export").clicked() {
                                action = Some(PanelAction::Export {
                                    session_id: session.id.clone(),
                                });
                            }
                            if ui.button("Delete").clicked() {
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
                            }
                        });
                    }
                }

                // Export/Import section
                ui.separator();
                ui.label("Export / Import");
                ui.horizontal(|ui| {
                    if ui.button("Export").clicked() {
                        if self.selected_id.is_some() {
                            action = Some(PanelAction::Export {
                                session_id: self.selected_id.clone().unwrap(),
                            });
                        }
                    }
                    if ui.button("Import").clicked() {
                        action = Some(PanelAction::Import);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Export path:");
                    ui.text_edit_singleline(&mut self.export_path);
                });
                ui.horizontal(|ui| {
                    ui.label("Import path:");
                    ui.text_edit_singleline(&mut self.import_path);
                });

                if self.creating {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.new_name);
                        if ui.button("Create").clicked() && !self.new_name.trim().is_empty() {
                            action = Some(PanelAction::Create(self.new_name.clone()));
                            self.new_name.clear();
                        }
                        if ui.button("Cancel").clicked() {
                            self.creating = false;
                            self.new_name.clear();
                        }
                    });
                }
            });

        // Apply actions after the UI closure to avoid borrow conflicts
        if let Some(act) = action {
            match act {
                PanelAction::Rename { id, new_name } => {
                    if let Some(mut s) = sessions::load_session(&self.sessions_dir, &id) {
                        s.name = new_name;
                        let _ = sessions::save_session(&self.sessions_dir, &s);
                    }
                    self.refresh();
                }
                PanelAction::Create(name) => {
                    let session = sessions::create_session(&self.sessions_dir, &name);
                    self.selected_id = Some(session.id.clone());
                    selected_id = Some(session.id.clone());
                    self.refresh();
                }
                PanelAction::Delete(id) => {
                    sessions::delete_session(&self.sessions_dir, &id);
                    if self.selected_id.as_deref() == Some(&id) {
                        self.selected_id = None;
                    }
                    self.refresh();
                }
                PanelAction::Export { session_id } => {
                    let output_path = if self.export_path.is_empty() {
                        PathBuf::from(format!("{}.json", session_id))
                    } else {
                        PathBuf::from(&self.export_path)
                    };
                    match sessions::export_session(&self.sessions_dir, &session_id, &output_path) {
                        Ok(()) => {
                            self.show_notification(&format!("Exported to {}", output_path.display()), true);
                        }
                        Err(e) => {
                            self.show_notification(&format!("Export failed: {}", e), false);
                        }
                    }
                }
                PanelAction::Import => {
                    let input_path = if self.import_path.is_empty() {
                        PathBuf::from("session.json")
                    } else {
                        PathBuf::from(&self.import_path)
                    };
                    match sessions::import_session(&self.sessions_dir, &input_path) {
                        Ok(new_id) => {
                            self.show_notification(&format!("Imported session: {}", new_id), true);
                            self.refresh();
                        }
                        Err(e) => {
                            self.show_notification(&format!("Import failed: {}", e), false);
                        }
                    }
                }
            }
        }

        selected_id
    }
}
