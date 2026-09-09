use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::config::{Config, ConnectionType};
use super::theme::Theme;

/// The settings dialog.
pub struct SettingsDialog {
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,
    pub system_prompt: String,
    pub theme: String,
    pub connection_type: String,
    pub remote_url: String,
    pub remote_api_key: String,
    pub encryption_enabled: bool,
    pub encryption_password: String,
    pub max_messages: usize,
    /// Shared flag to signal the app to open the presets dialog.
    pub show_presets: Arc<Mutex<bool>>,
    /// Shared config handle (written back on Save/Reset).
    config: Arc<Mutex<Config>>,
}

impl SettingsDialog {
    pub fn new(config: &Arc<Mutex<Config>>) -> Self {
        Self::new_with_presets_flag(config, Arc::new(Mutex::new(false)))
    }

    pub fn new_with_presets_flag(
        config: &Arc<Mutex<Config>>,
        show_presets: Arc<Mutex<bool>>,
    ) -> Self {
        let cfg = config.lock().unwrap();
        Self {
            server_path: cfg.server_path.clone(),
            model_path: cfg.model_path.clone(),
            port: cfg.port,
            n_gpu_layers: cfg.n_gpu_layers,
            n_ctx: cfg.n_ctx,
            threads: cfg.threads,
            system_prompt: cfg.system_prompt.clone(),
            theme: cfg.theme.clone(),
            connection_type: match &cfg.connection_type {
                ConnectionType::Local => "local".to_string(),
                ConnectionType::Remote => "remote".to_string(),
            },
            remote_url: cfg.remote_url.clone(),
            remote_api_key: cfg.remote_api_key.clone().unwrap_or_default(),
            encryption_enabled: cfg.encryption_enabled,
            encryption_password: cfg.encryption_password.clone().unwrap_or_default(),
            max_messages: cfg.max_messages,
            show_presets,
            config: config.clone(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let theme = Theme::from_name(&self.theme);
        let mut closed = false;
        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                ui.style_mut().spacing.item_spacing.y = 6.0;
                
                // Section: Server
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Server").strong().color(theme.primary));
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Server path:").size(12.0).color(theme.text_secondary));
                        if ui.button("Browse...").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("executable", &[""])
                                .pick_file() {
                                self.server_path = path.to_string_lossy().to_string();
                            }
                        }
                    });
                    ui.text_edit_singleline(&mut self.server_path);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Model path:").size(12.0).color(theme.text_secondary));
                        if ui.button("Browse...").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("gguf models", &["gguf"])
                                .pick_file() {
                                self.model_path = path.to_string_lossy().to_string();
                            }
                        }
                    });
                    ui.text_edit_singleline(&mut self.model_path);
                    ui.add(egui::Slider::new(&mut self.port, 1..=65535).text("Port"));
                    ui.add(egui::Slider::new(&mut self.n_gpu_layers, -1..=100).text("GPU layers"));
                    ui.add(egui::Slider::new(&mut self.n_ctx, 256..=1048576).text("Context size"));
                    ui.add(egui::Slider::new(&mut self.threads, 1..=16).text("Threads"));
                });

                ui.separator();

                // Section: Chat
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Chat").strong().color(theme.primary));
                    });
                    ui.separator();
                    ui.label(egui::RichText::new("System prompt:").size(12.0).color(theme.text_secondary));
                    ui.text_edit_multiline(&mut self.system_prompt);
                    ui.add(egui::Slider::new(&mut self.max_messages, 10..=500).text("Max messages to keep"));
                });

                ui.separator();

                // Section: Connection
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Connection").strong().color(theme.primary));
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Connection type:").size(12.0).color(theme.text_secondary));
                        ui.radio_value(&mut self.connection_type, "local".to_string(), "Local");
                        ui.radio_value(&mut self.connection_type, "remote".to_string(), "Remote");
                    });
                    if self.connection_type == "remote" {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Remote URL:").size(12.0).color(theme.text_secondary));
                            ui.text_edit_singleline(&mut self.remote_url);
                        });
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("API Key:").size(12.0).color(theme.text_secondary));
                            ui.text_edit_singleline(&mut self.remote_api_key);
                        });
                    }
                });

                ui.separator();

                // Section: Appearance
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Appearance").strong().color(theme.primary));
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Theme:").size(12.0).color(theme.text_secondary));
                        ui.radio_value(&mut self.theme, "dark".to_string(), "Dark");
                        ui.radio_value(&mut self.theme, "light".to_string(), "Light");
                    });
                });

                ui.separator();

                // Section: Encryption
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Encryption").strong().color(theme.primary));
                    });
                    ui.separator();
                    ui.add(egui::Checkbox::new(&mut self.encryption_enabled, "Enable encryption for sessions"));
                    if self.encryption_enabled {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Password:").size(12.0).color(theme.text_secondary));
                            ui.text_edit_singleline(&mut self.encryption_password);
                        });
                    }
                });

                ui.separator();

                // Presets button — signals the app to open the presets dialog
                if ui.button("Presets...").clicked() {
                    if let Ok(mut flag) = self.show_presets.lock() {
                        *flag = true;
                    }
                }

                ui.separator();

                // Action buttons
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("Save")
                        .fill(theme.primary)
                        .corner_radius(6)
                    ).clicked() {
                        self.save(self.config.clone());
                        closed = true;
                    }
                    if ui.add(egui::Button::new("Reset")
                        .fill(theme.surface_light)
                        .corner_radius(6)
                    ).clicked() {
                        *self = SettingsDialog::new(&self.config);
                    }
                    if ui.add(egui::Button::new("Close")
                        .fill(theme.surface_light)
                        .corner_radius(6)
                    ).clicked() {
                        closed = true;
                    }
                });
            });
        closed
    }

    pub fn save(&mut self, config: Arc<Mutex<Config>>) {
        let mut cfg = config.lock().unwrap();
        cfg.server_path.clone_from(&self.server_path);
        cfg.model_path.clone_from(&self.model_path);
        cfg.port = self.port;
        cfg.n_gpu_layers = self.n_gpu_layers;
        cfg.n_ctx = self.n_ctx;
        cfg.threads = self.threads;
        cfg.system_prompt.clone_from(&self.system_prompt);
        cfg.theme.clone_from(&self.theme);
        cfg.max_messages = self.max_messages;
        cfg.connection_type = match self.connection_type.as_str() {
            "remote" => ConnectionType::Remote,
            _ => ConnectionType::Local,
        };
        cfg.remote_url.clone_from(&self.remote_url);
        if !self.remote_api_key.is_empty() {
            cfg.remote_api_key = Some(self.remote_api_key.clone());
        } else {
            cfg.remote_api_key = None;
        }
        cfg.encryption_enabled = self.encryption_enabled;
        if !self.encryption_password.is_empty() {
            cfg.encryption_password = Some(self.encryption_password.clone());
        } else {
            cfg.encryption_password = None;
        }
        // Save via the shared config reference
        if let Err(e) = cfg.save() {
            eprintln!("Failed to save config: {}", e);
        }
    }
}
