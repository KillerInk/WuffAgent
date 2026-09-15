use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::config::{get_presets_path, Config, PresetStore, SearchBackend};
use super::theme::Theme;

/// The settings dialog.
pub struct SettingsDialog {
    pub system_prompt: String,
    pub theme: String,
    pub max_messages: usize,
    /// Saved presets (loaded from disk on open).
    presets: PresetStore,
    /// Name of the preset to apply on Save (if any).
    selected_preset: Option<String>,
    /// Shared flag to signal the app to open the presets dialog.
    pub show_presets: Arc<Mutex<bool>>,
    /// Shared config handle (written back on Save/Reset).
    pub(crate) config: Arc<Mutex<Config>>,
    /// Set when the dialog's config changed (Save) so the app can sync it back.
    pub(crate) config_dirty: bool,
    /// Web-search backend selection, as the combo-box label.
    search_backend_label: String,
    /// SearXNG instance URL (shown only for the SearXNG backend).
    searxng_url: String,
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
        let presets = PresetStore::load(&get_presets_path()).unwrap_or_default();
        let (search_backend_label, searxng_url) = match &cfg.search_config.backend {
            SearchBackend::Auto => ("Auto".to_string(), String::new()),
            SearchBackend::Bing => ("Bing".to_string(), String::new()),
            SearchBackend::Yahoo => ("Yahoo".to_string(), String::new()),
            SearchBackend::DuckDuckGo => ("DuckDuckGo".to_string(), String::new()),
            SearchBackend::SearXNG { base_url } => ("SearXNG".to_string(), base_url.clone()),
            // Compat backend: never re-selectable, keep the stored key.
            SearchBackend::Brave { .. } => ("Brave (API key)".to_string(), String::new()),
        };
        Self {
            system_prompt: cfg.system_prompt.clone(),
            theme: cfg.theme.clone(),
            max_messages: cfg.max_messages,
            presets,
            selected_preset: None,
            show_presets,
            config: config.clone(),
            config_dirty: false,
            search_backend_label,
            searxng_url,
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
                // Section: Presets (server/connection settings live here)
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Presets").strong().color(theme.primary));
                    });
                    ui.separator();
                    if self.presets.presets.is_empty() {
                        ui.label(egui::RichText::new("No presets saved yet. Click \"Manage Presets…\" to create one.")
                            .size(12.0).color(theme.text_secondary));
                    } else {
                        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                            for preset in self.presets.presets.iter() {
                                let label = format!("{} ({})", preset.name(), preset.preset_type());
                                let selected = self.selected_preset.as_deref() == Some(preset.name());
                                let btn = if selected {
                                    egui::Button::new(label).fill(theme.primary)
                                } else {
                                    egui::Button::new(label)
                                };
                                if ui.add(btn).clicked() {
                                    self.selected_preset =
                                        Some(preset.name().to_string());
                                }
                            }
                        });
                        if self.selected_preset.is_some()
                            && ui.button("Clear selection").clicked()
                        {
                            self.selected_preset = None;
                        }
                    }
                    if ui.button("Manage Presets…").clicked() {
                        if let Ok(mut flag) = self.show_presets.lock() {
                            *flag = true;
                        }
                    }
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

                // Section: Web search
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Web search").strong().color(theme.primary));
                    });
                    ui.separator();

                    // Combo box options; "Brave (API key)" is only offered when
                    // the config already uses it (compat, never re-created here).
                    let options: Vec<String> =
                        ["Auto", "Bing", "Yahoo", "DuckDuckGo", "SearXNG"].iter().map(|s| s.to_string()).collect();
                    let options = if self.search_backend_label == "Brave (API key)" {
                        let mut o = options;
                        o.push("Brave (API key)".to_string());
                        o
                    } else {
                        options
                    };
                    let mut selected = options
                        .iter()
                        .position(|o| *o == self.search_backend_label)
                        .unwrap_or(0);
                    // egui 0.36: ComboBox is a widget struct (no Ui::combo_box);
                    // default CloseOnClick closes the popup when an item is picked.
                    egui::ComboBox::from_label("Backend:")
                        .selected_text(self.search_backend_label.clone())
                        .show_index(ui, &mut selected, options.len(), |i| options[i].clone());
                    self.search_backend_label = options[selected].clone();

                    if self.search_backend_label == "SearXNG" {
                        ui.horizontal(|ui| {
                            ui.label("SearXNG URL:");
                            ui.text_edit_singleline(&mut self.searxng_url);
                        });
                    }

                    ui.label(egui::RichText::new("Backend changes take effect when the app restarts.")
                        .size(12.0).color(theme.text_secondary));
                });

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
        // Apply the selected preset (server/connection settings live in presets).
        if let Some(ref name) = self.selected_preset {
            if let Err(e) = self.presets.apply(name, &mut cfg) {
                eprintln!("Failed to apply preset '{}': {}", name, e);
            }
        }
        cfg.system_prompt.clone_from(&self.system_prompt);
        cfg.theme.clone_from(&self.theme);
        cfg.max_messages = self.max_messages;

        // Web search backend (env-var overrides are applied at tool
        // registration, i.e. on restart).
        cfg.search_config.backend = match self.search_backend_label.as_str() {
            "Auto" => SearchBackend::Auto,
            "Bing" => SearchBackend::Bing,
            "Yahoo" => SearchBackend::Yahoo,
            "DuckDuckGo" => SearchBackend::DuckDuckGo,
            "SearXNG" => {
                let url = self.searxng_url.trim().to_string();
                if url.is_empty() {
                    // No URL given — keep the previously configured backend
                    // rather than saving a SearXNG entry that cannot work.
                    cfg.search_config.backend.clone()
                } else {
                    SearchBackend::SearXNG { base_url: url }
                }
            }
            // "Brave (API key)": keep the stored key unchanged.
            _ => cfg.search_config.backend.clone(),
        };
        // Save via the shared config reference
        if let Err(e) = cfg.save() {
            eprintln!("Failed to save config: {}", e);
            return;
        }
        self.config_dirty = true;
    }
}
