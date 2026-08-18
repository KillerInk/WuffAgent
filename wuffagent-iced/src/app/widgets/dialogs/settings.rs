use iced::widget::{
    button, column, container, row, text, text_input, toggle, horizontal_rule,
};
use iced::{Alignment, Element, Length};
use std::sync::{Arc, Mutex};

use crate::app::messages::Message;
use crate::app::backend::Backend;
use iced::theme::Palette;
use crate::config::{Config, ConnectionType, PresetStore};

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
    pub show_presets: bool,
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
            show_presets: false,
        }
    }

    pub fn view(&self, pal: Palette) -> Element<'static, Message> {
        let section_title = |label: &str| {
            text(label)
                .size(13)
                .color(pal.primary)
                .font(iced::font::Font::DEFAULT)
        };

        let input_field = |label: &str, value: &str, msg: Message| {
            column!()
                .push(text(label).size(11).color(pal.text))
                .push(text_input("", value)
                    .on_input(move |v| msg)
                    .padding([6, 8])
                    .width(Length::Fill))
                .spacing(2)
        };

        let content = column!()
            .push(text("Settings").size(16).color(pal.text))
            .push(horizontal_rule(1).padding([4, 0]))

            // Server section
            .push(section_title("Server"))
            .push(input_field("Server path", &self.server_path, Message::SettingsServerPath))
            .push(input_field("Model path", &self.model_path, Message::SettingsModelPath))
            .push(input_field("Port", &self.port.to_string(), Message::SettingsPort))
            .push(input_field("GPU layers", &self.n_gpu_layers.to_string(), Message::SettingsGpuLayers))
            .push(input_field("Context size", &self.n_ctx.to_string(), Message::SettingsNCtx))
            .push(input_field("Threads", &self.threads.to_string(), Message::SettingsThreads))
            .push(horizontal_rule(1).padding([8, 0]))

            // Chat section
            .push(section_title("Chat"))
            .push(text("System prompt").size(11).color(pal.text))
            .push(text_input("", &self.system_prompt)
                .on_input(Message::SettingsSystemPrompt)
                .padding([6, 8])
                .width(Length::Fill))
            .push(row!()
                .push(toggle(None, self.streaming)
                    .on_toggle(|v| Message::SettingsStreaming(v)))
                .push(text("Stream responses").size(11).color(pal.text))
                .align_y(Alignment::Center)
                .spacing(6))
            .push(input_field("Max messages", &self.max_messages.to_string(), Message::SettingsMaxMessages))
            .push(horizontal_rule(1).padding([8, 0]))

            // Connection section
            .push(section_title("Connection"))
            .push(row!()
                .push(button(text(if self.connection_type == "local" { "● Local" } else { "○ Local" }))
                    .on_press(Message::SettingsConnectionType("local".to_string()))
                    .padding([4, 8]))
                .push(button(text(if self.connection_type == "remote" { "● Remote" } else { "○ Remote" }))
                    .on_press(Message::SettingsConnectionType("remote".to_string()))
                    .padding([4, 8]))
                .spacing(8))
            .push(if self.connection_type == "remote" {
                column!()
                    .push(input_field("Remote URL", &self.remote_url, Message::SettingsRemoteUrl))
                    .push(input_field("API Key", &self.remote_api_key, Message::SettingsRemoteApiKey))
                    .into()
            } else {
                column!().into()
            })
            .push(horizontal_rule(1).padding([8, 0]))

            // Appearance
            .push(section_title("Appearance"))
            .push(row!()
                .push(button(text(if self.theme == "dark" { "● Dark" } else { "○ Dark" }))
                    .on_press(Message::SettingsTheme("dark".to_string()))
                    .padding([4, 8]))
                .push(button(text(if self.theme == "light" { "● Light" } else { "○ Light" }))
                    .on_press(Message::SettingsTheme("light".to_string()))
                    .padding([4, 8]))
                .spacing(8))
            .push(horizontal_rule(1).padding([8, 0]))

            // Encryption
            .push(section_title("Encryption"))
            .push(row!()
                .push(toggle(None, self.encryption_enabled)
                    .on_toggle(|v| Message::SettingsEncryptionEnabled(v)))
                .push(text("Enable encryption for sessions").size(11).color(pal.text))
                .align_y(Alignment::Center)
                .spacing(6))
            .push(if self.encryption_enabled {
                input_field("Password", &self.encryption_password, Message::SettingsEncryptionPassword).into()
            } else {
                column!().into()
            })
            .push(horizontal_rule(1).padding([8, 0]))

            // Presets button
            .push(row!()
                .push(button(text("Presets..."))
                    .on_press(Message::SettingsShowPresets)
                    .padding([6, 12]))
                .push(row!()
                    .push(button(text("Save"))
                        .on_press(Message::SettingsSaved)
                        .padding([6, 16]))
                    .push(button(text("Close"))
                        .on_press(Message::SettingsClosed)
                        .padding([6, 16]))
                    .spacing(8))
                .push(iced::widget::horizontal_space())
                .align_y(Alignment::Center)
                .spacing(8))
            .padding([16, 16])
            .width(460);

        container(content)
            .width(460)
            .into()
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

// ─── Message variants for settings dialog ─────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SettingsMessage {
    ServerPath(String),
    ModelPath(String),
    Port(String),
    GpuLayers(String),
    NCtx(String),
    Threads(String),
    SystemPrompt(String),
    Streaming(bool),
    Theme(String),
    ConnectionType(String),
    RemoteUrl(String),
    RemoteApiKey(String),
    EncryptionEnabled(bool),
    EncryptionPassword(String),
    MaxMessages(String),
    ShowPresets,
}

impl From<SettingsMessage> for Message {
    fn from(msg: SettingsMessage) -> Self {
        match msg {
            SettingsMessage::ServerPath(v) => Message::SettingsServerPath(v),
            SettingsMessage::ModelPath(v) => Message::SettingsModelPath(v),
            SettingsMessage::Port(v) => Message::SettingsPort(v),
            SettingsMessage::GpuLayers(v) => Message::SettingsGpuLayers(v),
            SettingsMessage::NCtx(v) => Message::SettingsNCtx(v),
            SettingsMessage::Threads(v) => Message::SettingsThreads(v),
            SettingsMessage::SystemPrompt(v) => Message::SettingsSystemPrompt(v),
            SettingsMessage::Streaming(v) => Message::SettingsStreaming(v),
            SettingsMessage::Theme(v) => Message::SettingsTheme(v),
            SettingsMessage::ConnectionType(v) => Message::SettingsConnectionType(v),
            SettingsMessage::RemoteUrl(v) => Message::SettingsRemoteUrl(v),
            SettingsMessage::RemoteApiKey(v) => Message::SettingsRemoteApiKey(v),
            SettingsMessage::EncryptionEnabled(v) => Message::SettingsEncryptionEnabled(v),
            SettingsMessage::EncryptionPassword(v) => Message::SettingsEncryptionPassword(v),
            SettingsMessage::MaxMessages(v) => Message::SettingsMaxMessages(v),
            SettingsMessage::ShowPresets => Message::SettingsShowPresets,
        }
    }
}
