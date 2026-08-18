use iced::widget::{button, column, container, row, text, text_input};
use iced::Element;
use std::sync::{Arc, Mutex};

use crate::app::messages::Message;
use iced::theme::Palette;
use crate::config::{LocalPreset, Preset, PresetStore, RemotePreset};

/// State for the presets dialog, mutable across update calls.
pub struct PresetsDialog {
    pub selected_index: Option<usize>,
    pub show_add_form: bool,
    pub new_type: PresetType,
    pub new_name: String,
    pub new_server_path: String,
    pub new_model_path: String,
    pub new_port: u16,
    pub new_n_gpu_layers: i32,
    pub new_n_ctx: u32,
    pub new_threads: u32,
    pub new_remote_url: String,
    pub new_remote_api_key: String,
    pub message: Option<String>,
    pub store: PresetStore,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PresetType {
    Local,
    Remote,
}

impl PresetsDialog {
    pub fn new(store: PresetStore) -> Self {
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
        }
    }

    pub fn view(&self, pal: Palette) -> Element<'_, Message> {
        let preset_list: Element<'_, Message> = if self.store.presets.is_empty() {
            text("No presets saved yet.").size(12).color(pal.text).into()
        } else {
            let mut col = column!().spacing(2);
            for (i, preset) in self.store.presets.iter().enumerate() {
                let name = preset.name().to_string();
                let ptype = preset.preset_type();
                let label = format!("{} ({})", name, ptype);
                let selected = self.selected_index == Some(i);
                col = col.push(
                    button(text(label).size(11).color(if selected { iced::Color::WHITE } else { pal.text }))
                        .on_press(Message::PresetsSelect(i))
                        .padding([4, 8])
                );
            }
            Element::from(col)
        };

        let actions: Element<'_, Message> = if let Some(i) = self.selected_index {
            row!()
                .push(button(text("Load")).on_press(Message::PresetsLoad(i)).padding([4, 12]))
                .push(button(text("Delete")).on_press(Message::PresetsDelete(i)).padding([4, 12]))
                .spacing(8)
                .into()
        } else {
            Element::from(row!())
        };

        let add_form: Element<'_, Message> = if self.show_add_form {
            let mut col = column!()
                .push(text("Add New Preset").size(13).color(pal.text));
            col = col.push(text_input("Name", &self.new_name).on_input(|v| Message::PresetsNewName(v)).padding([4, 8]));
            col = col.push(
                row!()
                    .push(button(text(if self.new_type == PresetType::Local { "● Local" } else { "○ Local" }))
                        .on_press(Message::PresetsNewType(PresetType::Local)).padding([4, 8]))
                    .push(button(text(if self.new_type == PresetType::Remote { "● Remote" } else { "○ Remote" }))
                        .on_press(Message::PresetsNewType(PresetType::Remote)).padding([4, 8]))
                    .spacing(8)
            );
            if self.new_type == PresetType::Local {
                col = col.push(text_input("Server path", &self.new_server_path).on_input(|v| Message::PresetsNewServerPath(v)).padding([4, 8]));
                col = col.push(text_input("Model path", &self.new_model_path).on_input(|v| Message::PresetsNewModelPath(v)).padding([4, 8]));
                col = col.push(text_input("Port", &self.new_port.to_string()).on_input(|v| Message::PresetsNewPort(v)).padding([4, 8]));
                col = col.push(text_input("GPU layers", &self.new_n_gpu_layers.to_string()).on_input(|v| Message::PresetsNewGpuLayers(v)).padding([4, 8]));
                col = col.push(text_input("Context size", &self.new_n_ctx.to_string()).on_input(|v| Message::PresetsNewNCtx(v)).padding([4, 8]));
                col = col.push(text_input("Threads", &self.new_threads.to_string()).on_input(|v| Message::PresetsNewThreads(v)).padding([4, 8]));
            } else {
                col = col.push(text_input("Remote URL", &self.new_remote_url).on_input(|v| Message::PresetsNewRemoteUrl(v)).padding([4, 8]));
                col = col.push(text_input("API Key", &self.new_remote_api_key).on_input(|v| Message::PresetsNewRemoteApiKey(v)).padding([4, 8]));
            }
            col = col.push(
                row!()
                    .push(button(text("Add Preset")).on_press(Message::PresetsAdd).padding([4, 12]))
                    .push(button(text("Cancel")).on_press(Message::PresetsCancelAdd).padding([4, 12]))
                    .spacing(8)
            );
            Element::from(col)
        } else {
            Element::from(column!())
        };

        let msg_text: Element<'_, Message> = match &self.message {
            Some(m) => {
                let color = if m.starts_with("Error") { pal.danger } else { pal.success };
                text(m.clone()).size(11).color(color).into()
            }
            None => Element::from(column!()),
        };

        let content = column!()
            .push(text("Presets").size(16).color(pal.text))
            .push(
                row!()
                    .push(button(text("+ New")).on_press(Message::PresetsNew).padding([4, 12]))
                    .push(iced::widget::text(""))
                    .push(button(text("Save Store")).on_press(Message::PresetsSaveStore).padding([4, 12]))
            )
            .push(preset_list)
            .push(actions)
            .push(add_form)
            .push(msg_text)
            .push(
                row!()
                    .push(iced::widget::text(""))
                    .push(button(text("Close")).on_press(Message::PresetsClosed).padding([4, 12]))
            )
            .padding([16, 16]).width(420);

        container(content).width(420).into()
    }

    pub fn load(&mut self, index: usize, config: &Arc<Mutex<crate::config::Config>>) {
        if let Some(preset) = self.store.presets.get(index) {
            let name = preset.name().to_string();
            let mut cfg = config.lock().unwrap();
            match self.store.apply(&name, &mut cfg) {
                Ok(()) => {
                    let _ = cfg.save();
                    self.message = Some(format!("Loaded preset: {}", name));
                }
                Err(e) => {
                    self.message = Some(format!("Error loading preset: {}", e));
                }
            }
        }
    }

    pub fn delete(&mut self, index: usize) {
        if let Some(preset) = self.store.presets.get(index) {
            let name = preset.name().to_string();
            match self.store.remove(&name) {
                Ok(()) => {
                    self.selected_index = None;
                    self.message = Some(format!("Deleted preset: {}", name));
                }
                Err(e) => {
                    self.message = Some(format!("Error deleting preset: {}", e));
                }
            }
        }
    }

    pub fn add(&mut self) {
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
        match self.store.add(preset) {
            Ok(()) => {
                self.message = Some(format!("Added preset: {}", self.new_name));
                self.show_add_form = false;
                self.clear_new_form();
            }
            Err(e) => {
                self.message = Some(format!("Error: {}", e));
            }
        }
    }

    pub fn save_store(&mut self) {
        let path = crate::config::get_presets_path();
        if let Err(e) = self.store.save(&path) {
            self.message = Some(format!("Error saving: {}", e));
        } else {
            self.message = Some("Presets saved to disk".to_string());
        }
    }

    pub fn clear_new_form(&mut self) {
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
