use eframe::egui;
use std::sync::{Arc, Mutex};

use wuffagent_core::config::{Config, LocalPreset, Preset, PresetStore, RemotePreset};

/// Which kind of preset is currently being created/edited.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PresetType {
    Local,
    Remote,
}

/// Dialog for managing presets (create, select, load, delete).
pub struct PresetsDialog {
    /// Which preset is currently selected (by index), if any.
    pub selected_index: Option<usize>,
    /// Whether the "add new" form is open.
    pub show_add_form: bool,
    /// Type for the new preset being added.
    pub(super) new_type: PresetType,
    /// Fields for the new preset.
    pub new_name: String,
    pub new_server_path: String,
    pub new_model_path: String,
    pub new_port: u16,
    pub new_n_gpu_layers: i32,
    pub new_n_ctx: u32,
    pub new_threads: u32,
    pub new_remote_url: String,
    pub new_remote_api_key: String,
    /// Error/success message to display.
    pub message: Option<String>,
    /// The store (loaded from disk on open).
    pub store: PresetStore,
    /// Shared config handle (written back when a preset is applied).
    pub(crate) config: Arc<Mutex<Config>>,
}

impl PresetsDialog {
    pub fn new(store: PresetStore, config: &Arc<Mutex<Config>>) -> Self {
        Self {
            selected_index: None,
            show_add_form: false,
            new_type: PresetType::Local,
            new_name: String::new(),
            new_server_path: String::new(),
            new_model_path: String::new(),
            new_port: 8080,
            new_n_gpu_layers: 99,
            new_n_ctx: 4096,
            new_threads: 8,
            new_remote_url: String::new(),
            new_remote_api_key: String::new(),
            message: None,
            store,
            config: config.clone(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut closed = false;
        egui::Window::new("Presets")
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                ui.style_mut().spacing.item_spacing.y = 6.0;

                self.draw_list(ui);
                ui.separator();
                self.draw_actions(ui, self.config.clone());
                if self.show_add_form {
                    ui.separator();
                    self.draw_add_form(ui);
                }
                if let Some(ref msg) = self.message {
                    ui.separator();
                    let color = if msg.starts_with("Error") {
                        egui::Color32::RED
                    } else {
                        egui::Color32::GREEN
                    };
                    ui.label(
                        egui::RichText::new(msg).color(color).size(12.0),
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Save Store").clicked() {
                        if let Ok(path) = std::env::current_exe() {
                            if let Some(dir) = path.parent() {
                                let presets_path = dir.join("presets.json");
                                if let Err(e) = self.store.save(&presets_path) {
                                    self.message = Some(format!("Error saving presets: {}", e));
                                } else {
                                    self.message = Some("Presets saved to disk".to_string());
                                }
                            }
                        }
                    }
                    if ui.button("Close").clicked() {
                        closed = true;
                    }
                });
            });
        closed
    }

    fn draw_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Presets").strong());
            if ui.button("+ New").clicked() {
                self.show_add_form = true;
                self.selected_index = None;
                self.clear_new_form();
            }
        });

        if self.store.presets.is_empty() {
            ui.label("No presets saved yet.");
            return;
        }

        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
            for (i, preset) in self.store.presets.iter().enumerate() {
                let name = preset.name();
                let ptype = preset.preset_type();
                let label = format!("{} ({})", name, ptype);
                let selected = self.selected_index == Some(i);
                if ui
                    .add(
                        egui::Button::new(label)
                            .fill(if selected {
                                ui.style().visuals.selection.bg_fill
                            } else {
                                ui.style().visuals.widgets.noninteractive.weak_bg_fill
                            }),
                    )
                    .clicked()
                {
                    if selected {
                        self.selected_index = None;
                    } else {
                        self.selected_index = Some(i);
                    }
                }
            }
        });
    }

    fn draw_actions(&mut self, ui: &mut egui::Ui, config: Arc<Mutex<Config>>) {
        ui.horizontal(|ui| {
            let has_selection = self.selected_index.is_some();
            if ui.add_enabled(has_selection, egui::Button::new("Load")).clicked() {
                if let Some(i) = self.selected_index {
                    let name = self.store.presets[i].name().to_string();
                    let mut cfg = config.lock().unwrap();
                    if let Err(e) = self.store.apply(&name, &mut cfg) {
                        self.message = Some(format!("Error loading preset: {}", e));
                    } else {
                        if let Err(e) = cfg.save() {
                            self.message = Some(format!("Error saving config: {}", e));
                        } else {
                            self.message = Some(format!("Loaded preset: {}", name));
                        }
                    }
                }
            }
            if ui.add_enabled(has_selection, egui::Button::new("Delete")).clicked() {
                if let Some(i) = self.selected_index {
                    let name = self.store.presets[i].name().to_string();
                    if let Err(e) = self.store.remove(&name) {
                        self.message = Some(format!("Error deleting preset: {}", e));
                    } else {
                        self.selected_index = None;
                        self.message = Some(format!("Deleted preset: {}", name));
                    }
                }
            }
        });
    }

    fn draw_add_form(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Add New Preset").strong());

        // Name
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut self.new_name);
        });

        // Type
        ui.horizontal(|ui| {
            ui.label("Type:");
            ui.radio_value(&mut self.new_type, PresetType::Local, "Local");
            ui.radio_value(&mut self.new_type, PresetType::Remote, "Remote");
        });

        // Local fields
        if matches!(self.new_type, PresetType::Local) {
            ui.horizontal(|ui| {
                ui.label("Server path:");
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("executable", &[""])
                        .pick_file()
                    {
                        self.new_server_path = path.to_string_lossy().to_string();
                    }
                }
            });
            ui.text_edit_singleline(&mut self.new_server_path);
            ui.horizontal(|ui| {
                ui.label("Model path:");
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("gguf models", &["gguf"])
                        .pick_file()
                    {
                        self.new_model_path = path.to_string_lossy().to_string();
                    }
                }
            });
            ui.text_edit_singleline(&mut self.new_model_path);
            ui.add(egui::Slider::new(&mut self.new_port, 1..=65535).text("Port"));
            ui.add(egui::Slider::new(&mut self.new_n_gpu_layers, -1..=100).text("GPU layers"));
            ui.add(egui::Slider::new(&mut self.new_n_ctx, 256..=1048576).text("Context size"));
            ui.add(egui::Slider::new(&mut self.new_threads, 1..=16).text("Threads"));
        }

        // Remote fields
        if matches!(self.new_type, PresetType::Remote) {
            ui.horizontal(|ui| {
                ui.label("Remote URL:");
                ui.text_edit_singleline(&mut self.new_remote_url);
            });
            ui.horizontal(|ui| {
                ui.label("API Key:");
                ui.text_edit_singleline(&mut self.new_remote_api_key);
            });
        }

        ui.horizontal(|ui| {
            if ui.button("Add Preset").clicked() {
                self.add_preset();
            }
            if ui.button("Cancel").clicked() {
                self.show_add_form = false;
            }
        });
    }

    fn add_preset(&mut self) {
        self.message = None;
        if self.new_name.is_empty() {
            self.message = Some("Error: Name is required".to_string());
            return;
        }

        let preset = match &self.new_type {
            PresetType::Local => Preset::Local(LocalPreset {
                name: self.new_name.clone(),
                server_path: self.new_server_path.clone(),
                model_path: self.new_model_path.clone(),
                port: self.new_port,
                n_gpu_layers: self.new_n_gpu_layers,
                n_ctx: self.new_n_ctx,
                threads: self.new_threads,
            }),
            PresetType::Remote => Preset::Remote(RemotePreset {
                name: self.new_name.clone(),
                remote_url: self.new_remote_url.clone(),
                remote_api_key: if self.new_remote_api_key.is_empty() {
                    None
                } else {
                    Some(self.new_remote_api_key.clone())
                },
            }),
        };

        if let Err(e) = self.store.add(preset) {
            self.message = Some(format!("Error: {}", e));
        } else {
            self.message = Some(format!("Added preset: {}", self.new_name));
            self.show_add_form = false;
            self.clear_new_form();
        }
    }

    fn clear_new_form(&mut self) {
        self.new_name.clear();
        self.new_server_path.clear();
        self.new_model_path.clear();
        self.new_port = 8080;
        self.new_n_gpu_layers = 99;
        self.new_n_ctx = 4096;
        self.new_threads = 8;
        self.new_remote_url.clear();
        self.new_remote_api_key.clear();
        self.new_type = PresetType::Local;
    }
}
