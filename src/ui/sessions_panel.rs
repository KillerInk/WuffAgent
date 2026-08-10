use eframe::egui;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use crate::config::Config;
use crate::sessions;

pub struct SessionsPanel {
    sessions: Vec<crate::sessions::Session>,
    selected_id: Option<String>,
    sessions_dir: PathBuf,
    config: Arc<Mutex<Config>>,
    renaming: Option<String>,
    rename_input: String,
    creating: bool,
    new_name: String,
    /// Session id to clear (set after user confirms clear dialog).
    pub(super) clear_session_id: Option<String>,
    /// If Some, holds the pending clear confirmation dialog state: (id, name, last_message).
    pending_clear: Option<(String, String, String)>,
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
    /// Pending context menu to show: (session_id, center_point).
    pending_menu: Option<(String, egui::Pos2)>,
}

#[derive(Debug)]
enum PanelAction {
    Rename { id: String, new_name: String },
    Create(String),
    Delete(String),
    Clear(String),
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
        let selected_id = sessions.first().map(|s| s.id.clone());
        Self {
            sessions,
            selected_id,
            sessions_dir: dir,
            config: config.clone(),
            renaming: None,
            rename_input: String::new(),
            creating: false,
            new_name: String::new(),
            clear_session_id: None,
            pending_clear: None,
            pending_delete: None,
            notification: None,
            notification_start: 0.0,
            export_path: String::new(),
            import_path: String::new(),
            pending_menu: None,
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

        let response = ui.add_sized(
            ui.available_size_before_wrap(),
            egui::Button::new(display_label).fill(if is_selected || is_renaming {
                egui::Color32::from_rgb(50, 50, 100)
            } else {
                egui::Color32::TRANSPARENT
            }).sense(egui::Sense::click())
        );
        response
    }
    
    fn draw_session_item(
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
        
        let label = format!(
            "{}\n{}  •  {}\n{}",
            session.name,
            count_text,
            preview_truncated,
            timestamp_text
        );
        
        let display_label = if is_renaming {
            format!("{}\n🔄 rename", label)
        } else {
            label
        };
        
        let response = ui.add(egui::Button::new(display_label).fill(if is_selected || is_renaming {
            egui::Color32::from_rgb(50, 50, 100)
        } else {
            egui::Color32::TRANSPARENT
        }).sense(egui::Sense::click()));
        
        response
    }

    pub fn draw(&mut self, ctx: &egui::Context) -> Option<String> {
        let mut selected_id: Option<String> = None;
        let mut action: Option<PanelAction> = None;

        // Show the clear confirmation dialog if pending
        if let Some((id, name, last_message)) = self.pending_clear.take() {
            let mut should_clear = None;
            let _ = egui::Window::new("Clear Session")
                .collapsible(false)
                .resizable(false)
                .movable(true)
                .default_pos([300.0, 200.0])
                .show(ctx, |ui| {
                    ui.weak("Are you sure you want to clear this session?");
                    ui.separator();
                    ui.label(format!("Session: {}", name));
                    if !last_message.is_empty() {
                        let truncated = truncate(&last_message, 50);
                        ui.weak(format!("Last message: {}", truncated));
                    }
                    ui.separator();
                    ui.label("This will delete the session file and all backups.");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Clear").clicked() {
                            should_clear = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            should_clear = Some(false);
                        }
                    });
                });
            match should_clear {
                Some(true) => {
                    action = Some(PanelAction::Clear(id));
                }
                Some(false) | None => {
                    self.pending_clear = None;
                }
            }
        }

        // Show the delete confirmation dialog if pending
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
                    eprintln!("[Clear] Button clicked, selected_id={:?}, sessions_count={}", self.selected_id, self.sessions.len());
                    // Prefer selected session from the panel; fall back to client's current session
                    let session_id = self.selected_id.clone();
                    let session = self.sessions.iter().find(|s| Some(&s.id) == session_id.as_ref())
                        .or_else(|| {
                            // Try to find by client's current session id
                            None
                        });
                    if let Some(session) = session {
                        eprintln!("[Clear] Found session: id={}, name={}", session.id, session.name);
                        let last_msg: String = session
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.role == "user" || m.role == "assistant")
                            .map(|m| m.content.clone())
                            .unwrap_or_default();
                        self.pending_clear = Some((
                            session.id.clone(),
                            session.name.clone(),
                            last_msg,
                        ));
                        eprintln!("[Clear] pending_clear set to Some");
                    } else {
                        eprintln!("[Clear] No session found for selected_id={:?}", session_id);
                        self.show_notification("No session selected", false);
                    }
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
                    let response = Self::draw_session_item(ui, session, is_selected, is_renaming);
                    
                    // Right-click to open context menu with export option
                    if response.secondary_clicked() {
                        // Store the menu to show on next frame
                        self.pending_menu = Some((session.id.clone(), response.rect.center()));
                    }
                    
                    // Show the pending context menu
                    if let Some((menu_session_id, center)) = self.pending_menu.take() {
                        if menu_session_id == session.id {
                            let menu_id = ui.make_persistent_id(format!("ctx_menu_{}", session.id));
                            egui::popup::popup_above_or_below_widget(
                                ui,
                                menu_id,
                                &response,
                                egui::AboveOrBelow::Below,
                                egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
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
                                },
                            );
                        }
                    }
                    
                    if response.clicked() {
                        selected_id = Some(session.id.clone());
                        self.selected_id = Some(session.id.clone());
                        // Update config so the selected session is loaded on next app start
                        {
                            let mut cfg = self.config.lock().unwrap();
                            cfg.session_id = Some(session.id.clone());
                            if let Err(e) = cfg.save() {
                                eprintln!("Failed to save config after selecting session: {}", e);
                            }
                        }
                    }
                    if response.double_clicked() {
                        self.renaming = Some(session.id.clone());
                        self.rename_input = session.name.clone();
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
                        let resp = ui.text_edit_singleline(&mut self.new_name);
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if !self.new_name.trim().is_empty() {
                                action = Some(PanelAction::Create(self.new_name.clone()));
                                self.new_name.clear();
                                self.creating = false;
                            }
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
                    // Update config so the new session is loaded on next app start
                    {
                        let mut cfg = self.config.lock().unwrap();
                        cfg.session_id = Some(session.id.clone());
                        if let Err(e) = cfg.save() {
                            eprintln!("Failed to save config after creating session: {}", e);
                        }
                    }
                    self.refresh();
                }
                PanelAction::Delete(id) => {
                    sessions::delete_session(&self.sessions_dir, &id);
                    if self.selected_id.as_deref() == Some(&id) {
                        self.selected_id = None;
                    }
                    self.refresh();
                }
                PanelAction::Clear(id) => {
                    eprintln!("[Clear] Action Clear triggered for id={}, sessions_dir={}", id, self.sessions_dir.display());
                    match sessions::clear_session_messages(&self.sessions_dir, &id) {
                        Ok(()) => {
                            eprintln!("[Clear] clear_session_messages succeeded");
                            self.clear_session_id = Some(id.clone());
                            self.refresh();
                            self.show_notification("Session cleared", true);
                        }
                        Err(e) => {
                            eprintln!("[Clear] clear_session_messages failed: {}", e);
                            self.show_notification(&format!("Clear failed: {}", e), false);
                        }
                    }
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
