use iced::widget::{button, column, container, row, text, text_input};
use iced::{Alignment, Element, Length};
use std::sync::{Arc, Mutex};

use crate::app::messages::Message;
use iced::theme::Palette;
use crate::config::{Config, ConnectionType};

/// State for the settings dialog, mutable across update calls.
pub struct SettingsDialog {
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,
    pub system_prompt: String,
    pub streaming: bool,
    pub theme: String,
    pub connection_type: String,
    pub remote_url: String,
    pub remote_api_key: String,
    pub encryption_enabled: bool,
    pub encryption_password: String,
    pub max_messages: usize,
}

impl SettingsDialog {
    pub fn new(config: &Config) -> Self {
        Self {
            server_path: config.server_path.clone(),
            model_path: config.model_path.clone(),
            port: config.port,
            n_gpu_layers: config.n_gpu_layers,
            n_ctx: config.n_ctx,
            threads: config.threads,
            system_prompt: config.system_prompt.clone(),
            streaming: config.streaming,
            theme: config.theme.clone(),
            connection_type: match &config.connection_type {
                ConnectionType::Local => "local".to_string(),
                ConnectionType::Remote => "remote".to_string(),
            },
            remote_url: config.remote_url.clone(),
            remote_api_key: config.remote_api_key.clone().unwrap_or_default(),
            encryption_enabled: config.encryption_enabled,
            encryption_password: config.encryption_password.clone().unwrap_or_default(),
            max_messages: config.max_messages,
        }
    }

    pub fn view(&self, pal: Palette) -> Element<'_, Message> {
        let section_title = |label: String| {
            text(label).size(13).color(pal.primary).font(iced::font::Font::DEFAULT)
        };

        let label_field = |label: String, value: String, msg_builder: fn(String) -> Message| {
            column!()
                .push(text(label).size(11).color(pal.text))
                .push(text_input("", &value).on_input(move |v| msg_builder(v)).padding([6, 8]).width(Length::Fill))
                .spacing(2)
        };

        let btn = |label: String, msg: Message| {
            button(text(label)).on_press(msg).padding([4, 8])
        };

        let remote_section = if self.connection_type == "remote" {
            column!()
                .push(text("Remote URL").size(11).color(pal.text))
                .push(text_input("", &self.remote_url).on_input(Message::SettingsRemoteUrl).padding([6, 8]).width(Length::Fill))
                .push(text("API Key").size(11).color(pal.text))
                .push(text_input("", &self.remote_api_key).on_input(Message::SettingsRemoteApiKey).padding([6, 8]).width(Length::Fill))
                .spacing(2)
        } else {
            column!()
        };

        let encryption_section = if self.encryption_enabled {
            column!()
                .push(text("Password").size(11).color(pal.text))
                .push(text_input("", &self.encryption_password).on_input(Message::SettingsEncryptionPassword).padding([6, 8]).width(Length::Fill))
                .spacing(2)
        } else {
            column!()
        };

        let content = column!()
            .push(text("Settings").size(16).color(pal.text))
            .push(text("").size(2))
            .push(section_title("Server".to_string()))
            .push(label_field("Server path".to_string(), self.server_path.clone(), Message::SettingsServerPath))
            .push(label_field("Model path".to_string(), self.model_path.clone(), Message::SettingsModelPath))
            .push(label_field("Port".to_string(), self.port.to_string(), Message::SettingsPort))
            .push(label_field("GPU layers".to_string(), self.n_gpu_layers.to_string(), Message::SettingsGpuLayers))
            .push(label_field("Context size".to_string(), self.n_ctx.to_string(), Message::SettingsNCtx))
            .push(label_field("Threads".to_string(), self.threads.to_string(), Message::SettingsThreads))
            .push(text("").size(2))
            .push(section_title("Chat".to_string()))
            .push(text("System prompt").size(11).color(pal.text))
            .push(text_input("", &self.system_prompt).on_input(Message::SettingsSystemPrompt).padding([6, 8]).width(Length::Fill))
            .push(row!()
                .push(btn(if self.streaming { "● Stream".to_string() } else { "○ Stream".to_string() }, Message::SettingsStreaming(true)))
                .push(text("Stream responses").size(11).color(pal.text))
                .align_y(Alignment::Center).spacing(6))
            .push(label_field("Max messages".to_string(), self.max_messages.to_string(), Message::SettingsMaxMessages))
            .push(text("").size(2))
            .push(section_title("Connection".to_string()))
            .push(row!()
                .push(btn(if self.connection_type == "local" { "● Local".to_string() } else { "○ Local".to_string() }, Message::SettingsConnectionType("local".to_string())))
                .push(btn(if self.connection_type == "remote" { "● Remote".to_string() } else { "○ Remote".to_string() }, Message::SettingsConnectionType("remote".to_string())))
                .spacing(8))
            .push(remote_section)
            .push(text("").size(2))
            .push(section_title("Appearance".to_string()))
            .push(row!()
                .push(btn(if self.theme == "dark" { "● Dark".to_string() } else { "○ Dark".to_string() }, Message::SettingsTheme("dark".to_string())))
                .push(btn(if self.theme == "light" { "● Light".to_string() } else { "○ Light".to_string() }, Message::SettingsTheme("light".to_string())))
                .spacing(8))
            .push(text("").size(2))
            .push(section_title("Encryption".to_string()))
            .push(row!()
                .push(btn(if self.encryption_enabled { "● Enable".to_string() } else { "○ Enable".to_string() }, Message::SettingsEncryptionEnabled(true)))
                .push(text("Enable encryption").size(11).color(pal.text))
                .align_y(Alignment::Center).spacing(6))
            .push(encryption_section)
            .push(text("").size(4))
            .push(row!()
                .push(btn("Presets...".to_string(), Message::SettingsShowPresets))
                .push(row!()
                    .push(btn("Save".to_string(), Message::SettingsSaved))
                    .push(btn("Close".to_string(), Message::SettingsClosed))
                    .spacing(8))
                .push(iced::widget::text(""))
                .align_y(Alignment::Center).spacing(8))
            .padding([16, 16]).width(460);

        container(content).width(460).into()
    }

    pub fn save(&self, config: &Arc<Mutex<Config>>) {
        let mut cfg = config.lock().unwrap();
        cfg.server_path = self.server_path.clone();
        cfg.model_path = self.model_path.clone();
        cfg.port = self.port;
        cfg.n_gpu_layers = self.n_gpu_layers;
        cfg.n_ctx = self.n_ctx;
        cfg.threads = self.threads;
        cfg.system_prompt = self.system_prompt.clone();
        cfg.streaming = self.streaming;
        cfg.theme = self.theme.clone();
        cfg.max_messages = self.max_messages;
        cfg.connection_type = match self.connection_type.as_str() {
            "remote" => ConnectionType::Remote,
            _ => ConnectionType::Local,
        };
        cfg.remote_url = self.remote_url.clone();
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
        let _ = cfg.save();
    }
}
