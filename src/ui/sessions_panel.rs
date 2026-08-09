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
}

#[derive(Debug)]
enum PanelAction {
    Rename { id: String, new_name: String },
    Create(String),
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
        }
    }

    pub fn refresh(&mut self) {
        self.sessions = sessions::list_sessions(&self.sessions_dir);
    }

    pub fn draw(&mut self, ctx: &egui::Context) -> Option<String> {
        let mut selected_id: Option<String> = None;
        let mut action: Option<PanelAction> = None;

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

                // Collect actions to avoid borrowing self inside the loop
                for session in &self.sessions {
                    if Some(&session.id) == self.renaming.as_ref() {
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut self.rename_input);
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
                    let response = ui.add(egui::Button::new(&session.name).fill(
                        if is_selected {
                            egui::Color32::from_rgb(50, 50, 100)
                        } else {
                            egui::Color32::TRANSPARENT
                        },
                    ));
                    if response.clicked() {
                        selected_id = Some(session.id.clone());
                        self.selected_id = Some(session.id.clone());
                    }
                    if response.double_clicked() {
                        self.renaming = Some(session.id.clone());
                        self.rename_input = session.name.clone();
                    }
                }

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
            }
        }

        if self.clear_action {
            self.clear_action = false;
        }

        selected_id
    }
}
