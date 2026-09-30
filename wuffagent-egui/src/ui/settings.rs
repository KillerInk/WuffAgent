use eframe::egui;
use std::sync::{Arc, Mutex};

use super::theme::Theme;
use wuffagent_core::config::{get_presets_path, Config, PresetStore, SearchBackend};

/// The settings dialog.
pub struct SettingsDialog {
    pub system_prompt: String,
    pub theme: String,
    pub max_messages: usize,
    /// Saved presets. A SHARED live handle (not a snapshot): the presets
    /// manager dialog and this list read/write the same store, so a preset
    /// added/deleted/saved there appears here on the next frame — no
    /// close-and-reopen of Settings needed.
    pub(crate) presets: Arc<Mutex<PresetStore>>,
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
    /// Full constructor. `presets` is a SHARED handle: window.rs passes the
    /// same store to the presets manager dialog so both UIs see one live
    /// store (add/delete/save in either is immediately visible in the other).
    /// A standalone caller loads the store from disk first.
    pub fn new_with_presets_flag_and_store(
        config: &Arc<Mutex<Config>>,
        show_presets: Arc<Mutex<bool>>,
        presets: Arc<Mutex<PresetStore>>,
    ) -> Self {
        let cfg = config.lock().unwrap();
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
                // One snapshot of the SHARED store per frame: the mutex is
                // never held across the egui closures, and a preset added in
                // the presets manager shows up here immediately (no reopen).
                // `active` marks the preset whose settings match the live
                // config, so the user sees which stored preset is in effect.
                let rows = self.preset_rows();
                // Section: Presets (server/connection settings live here)
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Presets").strong().color(theme.primary));
                    });
                    ui.separator();
                    if rows.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "No presets saved yet. Click \"Manage Presets…\" to create one.",
                            )
                            .size(12.0)
                            .color(theme.text_secondary),
                        );
                    } else {
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .show(ui, |ui| {
                                for (name, label, selected, _active) in rows.iter() {
                                    let btn = if *selected {
                                        egui::Button::new(label).fill(theme.primary)
                                    } else {
                                        egui::Button::new(label)
                                    };
                                    if ui.add(btn).clicked() {
                                        self.selected_preset = Some(name.clone());
                                    }
                                }
                            });
                        if self.selected_preset.is_some() && ui.button("Clear selection").clicked()
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
                    ui.label(
                        egui::RichText::new("System prompt:")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    ui.text_edit_multiline(&mut self.system_prompt);
                    ui.add(
                        egui::Slider::new(&mut self.max_messages, 10..=500)
                            .text("Max messages to keep"),
                    );
                });

                ui.separator();

                // Section: Web search
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Web search")
                                .strong()
                                .color(theme.primary),
                        );
                    });
                    ui.separator();

                    // Combo box options; "Brave (API key)" is only offered when
                    // the config already uses it (compat, never re-created here).
                    let options: Vec<String> = ["Auto", "Bing", "Yahoo", "DuckDuckGo", "SearXNG"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
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

                    ui.label(
                        egui::RichText::new("Backend changes take effect when the app restarts.")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                });

                ui.separator();

                // Action buttons
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new("Save")
                                .fill(theme.primary)
                                .corner_radius(6),
                        )
                        .clicked()
                    {
                        self.save(self.config.clone());
                        closed = true;
                    }
                    if ui
                        .add(
                            egui::Button::new("Reset")
                                .fill(theme.surface_light)
                                .corner_radius(6),
                        )
                        .clicked()
                    {
                        // Keep the SHARED preset store and the presets flag:
                        // a fresh disk snapshot would desync the presets
                        // manager if it is open.
                        *self = SettingsDialog::new_with_presets_flag_and_store(
                            &self.config,
                            self.show_presets.clone(),
                            self.presets.clone(),
                        );
                    }
                    if ui
                        .add(
                            egui::Button::new("Close")
                                .fill(theme.surface_light)
                                .corner_radius(6),
                        )
                        .clicked()
                    {
                        closed = true;
                    }
                });
            });
        closed
    }

    /// One row per preset in the shared store: `(name, label, selected, active)`.
    ///
    /// `active` is computed against the LIVE config (`Preset::matches`), so
    /// the list marks the preset the app is currently running with — it
    /// updates automatically after a Load (presets dialog) or Save (here).
    fn preset_rows(&self) -> Vec<(String, String, bool, bool)> {
        let cfg = self.config.lock().unwrap();
        let store = self.presets.lock().unwrap();
        store
            .presets
            .iter()
            .map(|p| {
                let active = p.matches(&*cfg);
                let mut label = format!("{} ({})", p.name(), p.preset_type());
                if active {
                    label.push_str(" (active)");
                }
                (
                    p.name().to_string(),
                    label,
                    self.selected_preset.as_deref() == Some(p.name()),
                    active,
                )
            })
            .collect()
    }

    pub fn save(&mut self, config: Arc<Mutex<Config>>) {
        let mut cfg = config.lock().unwrap();
        // Apply the selected preset (server/connection settings live in presets).
        if let Some(ref name) = self.selected_preset {
            let err = self.presets.lock().unwrap().apply(name, &mut cfg);
            if let Err(e) = err {
                tracing::warn!(preset = %name, error = %e, "Failed to apply preset");
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
            tracing::warn!(error = %e, "Failed to save config");
            return;
        }
        self.config_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuffagent_core::config::{Config, ConnectionType, LocalPreset, Preset, RemotePreset};

    fn dialog_with(config: Config, presets: Vec<Preset>) -> (SettingsDialog, Arc<Mutex<PresetStore>>) {
        let store = Arc::new(Mutex::new(PresetStore { presets }));
        let d = SettingsDialog::new_with_presets_flag_and_store(
            &Arc::new(Mutex::new(config)),
            Arc::new(Mutex::new(false)),
            store.clone(),
        );
        (d, store)
    }

    fn local_config() -> Config {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "C:\\llama\\server.exe".to_string();
        cfg.model_path = "C:\\llama\\model.gguf".to_string();
        cfg.port = 8081;
        cfg.n_gpu_layers = 45;
        cfg.n_ctx = 8192;
        cfg.threads = 12;
        cfg
    }

    fn matching_preset(name: &str) -> Preset {
        Preset::Local(LocalPreset {
            name: name.to_string(),
            server_path: "C:\\llama\\server.exe".to_string(),
            model_path: "C:\\llama\\model.gguf".to_string(),
            port: 8081,
            n_gpu_layers: 45,
            n_ctx: 8192,
            threads: 12,
        })
    }

    #[test]
    fn preset_rows_marks_the_active_preset() {
        let mut inactive = matching_preset("other-server");
        if let Preset::Local(lp) = &mut inactive {
            lp.port = 9999; // connection fields must actually differ (name is not compared)
        }
        let (d, _store) = dialog_with(local_config(), vec![matching_preset("active-one"), inactive]);
        let rows = d.preset_rows();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].3, "the matching preset must be marked active");
        assert!(rows[0].1.ends_with("(active)"), "label carries the marker: {:?}", rows[0].1);
        assert!(!rows[1].3, "a preset with different connection settings stays inactive");
        assert!(!rows[1].1.contains("(active)"));
    }

    #[test]
    fn preset_rows_reflects_shared_store_without_reopening() {
        let (mut d, store) = dialog_with(Config::default(), vec![]);
        assert!(
            d.preset_rows().is_empty(),
            "empty store starts empty"
        );
        // The presets manager adds a preset to the SHARED store — the
        // settings list must pick it up on the next frame (the reported bug:
        // "i only see it when i close and reopen the settings").
        store
            .lock()
            .unwrap()
            .add(Preset::Remote(RemotePreset {
                name: "new-remote".to_string(),
                remote_url: "http://example".to_string(),
                remote_api_key: None,
            }))
            .unwrap();
        let rows = d.preset_rows();
        assert_eq!(rows.len(), 1, "new preset must appear without reopening");
        assert_eq!(rows[0].0, "new-remote");
        // Selection is stored by name and round-trips into the row state.
        d.selected_preset = Some("new-remote".to_string());
        assert!(d.preset_rows()[0].2, "selected row is flagged selected");
    }
}
