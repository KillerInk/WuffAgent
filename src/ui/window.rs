use eframe::egui;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::{ChatClient, Usage};
use crate::config::{ChatMessage as ConfigChatMessage, Config};
use crate::server::ServerManager;
use crate::ui::settings::SettingsDialog;

#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Stopped,
    Connecting,
    Ready,
    Generating,
    Error(String),
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub enum AppEvent {
    MessageResult { content: String, usage: Option<Usage> },
    MessageError { error: String },
    StreamChunk { content: String },
    StreamComplete { content: String, usage: Option<Usage> },
    StreamError { error: String },
}

pub struct ChatApp {
    server: Arc<ServerManager>,
    client: Arc<Mutex<ChatClient>>,
    config: Arc<Mutex<Config>>,
    pending_tx: Option<mpsc::Sender<AppEvent>>,
    pending_rx: Mutex<mpsc::Receiver<AppEvent>>,

    // UI state
    chat_display: Vec<ChatMessage>,
    input_text: String,
    is_generating: bool,
    status: AppStatus,
    streaming: bool,
    show_settings: bool,
    settings_dialog: Option<SettingsDialog>,
    progress: f32,
    pending_error: Option<String>,

    // For streaming
    current_response: String,
    streaming_task: Option<JoinHandle<()>>,

    // Stats for bottom bar
    token_count: u32,
    context_used: f32,

    // Remote server n_ctx (fetched from /props), 0 = not yet fetched
    remote_n_ctx: u32,

    // Shared Arc for the background fetch task to update
    remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,

    // Shared handle for background remote n_ctx fetch task
    remote_n_ctx_handle: Option<JoinHandle<()>>,

    // Max messages to keep in display (truncate for context window)
    max_display_messages: usize,
}

impl ChatApp {
    pub fn new(
        server: Arc<ServerManager>,
        client: Arc<Mutex<ChatClient>>,
        config: Arc<Mutex<Config>>,
    ) -> Self {
        let cfg = config.lock().unwrap();
        let streaming = cfg.streaming;
        let chat_history: Vec<ChatMessage> = cfg.chat_history.clone().into_iter().map(|m| ChatMessage {
            role: m.role,
            content: m.content,
        }).collect();
        drop(cfg);
        let (tx, rx) = mpsc::channel();
        Self {
            server,
            client,
            config,
            pending_tx: Some(tx),
            pending_rx: Mutex::new(rx),
            chat_display: chat_history,
            input_text: String::new(),
            is_generating: false,
            status: AppStatus::Stopped,
            streaming,
            show_settings: false,
            settings_dialog: None,
            progress: 0.0,
            pending_error: None,
            current_response: String::new(),
            streaming_task: None,
            token_count: 0,
            context_used: 0.0,
            remote_n_ctx: 0,
            remote_n_ctx_arc: None,
            remote_n_ctx_handle: None,
            max_display_messages: 100,
        }
    }

    fn setup_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("WuffAgent");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Theme").clicked() {
                        self.toggle_theme(ctx);
                    }
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                    }
                });
            });
        });

        // Bottom panels stack upward, so bottom_bar must be declared first to be at the bottom
        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            self.draw_status_bar(ui);
            ui.separator();
            self.draw_bottom_bar(ui);
        });

        // Input area is its own bottom panel, anchored above the status bar
        egui::TopBottomPanel::bottom("input_panel")
            .default_height(50.0)
            .resizable(false)
            .show(ctx, |ui| {
                self.draw_input_area(ui);
            });

        // Chat area fills all remaining space between top bar and input panel
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("chat_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Show pending error as inline warning
                    if let Some(ref err) = self.pending_error {
                        let err_clone = err.clone();
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::RED, format!("Error: {}", err_clone));
                            if ui.button("Dismiss").clicked() {
                                self.pending_error = None;
                            }
                        });
                        ui.separator();
                    }

                    // Clone messages to avoid borrow checker issues
                    let messages: Vec<ChatMessage> = self.chat_display.clone();
                    ui.vertical(|ui| {
                        for msg in &messages {
                            self.draw_message(ui, msg);
                        }

                        // Show current streaming response
                        if self.is_generating && !self.current_response.is_empty() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label("AI:  ");
                                ui.label(
                                    egui::RichText::new(&self.current_response)
                                        .color(egui::Color32::from_rgb(150, 200, 150)),
                                );
                                ui.spinner();
                            });
                        } else if self.is_generating && self.current_response.is_empty() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label("AI:  ");
                                ui.spinner();
                            });
                        }
                    });
                });
        });
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        let cfg = self.config.lock().unwrap();
        let current_theme = cfg.theme.clone();
        drop(cfg);

        let new_theme = if current_theme == "dark" {
            "light".to_string()
        } else {
            "dark".to_string()
        };

        self.config.lock().unwrap().theme = new_theme.clone();
        if let Err(e) = self.config.lock().unwrap().save() {
            eprintln!("Failed to save theme: {}", e);
        }

        let visuals = if new_theme == "dark" {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        ctx.set_visuals(visuals);
    }

    fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        // Show pending error as inline warning
        if let Some(ref err) = self.pending_error {
            let err_clone = err.clone();
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", err_clone));
                if ui.button("Dismiss").clicked() {
                    self.pending_error = None;
                }
            });
            ui.separator();
        }

        // Clone messages to avoid borrow checker issues
        let messages: Vec<ChatMessage> = self.chat_display.clone();

        egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    for msg in &messages {
                        self.draw_message(ui, msg);
                    }

                    // Show current streaming response
                    if self.is_generating && !self.current_response.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label("AI:  ");
                            ui.label(
                                egui::RichText::new(&self.current_response)
                                    .color(egui::Color32::from_rgb(150, 200, 150)),
                            );
                            ui.spinner();
                        });
                    } else if self.is_generating && self.current_response.is_empty() {
                        // Show spinner while waiting for first chunk
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label("AI:  ");
                            ui.spinner();
                        });
                    }
                });
            });
    }

    fn draw_message(&self, ui: &mut egui::Ui, message: &ChatMessage) {
        ui.horizontal(|ui| {
            if message.role == "user" {
                ui.label("You: ");
            } else {
                ui.label("AI: ");
            }
            ui.label(&message.content);
        });
    }

    fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        ui.style_mut().spacing.item_spacing.y = 0.0;

        // Validate input length
        const MAX_MESSAGE_LENGTH: usize = 4000;
        let input_len = self.input_text.len();
        if input_len > MAX_MESSAGE_LENGTH {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::RED,
                    format!("Message too long (max {} characters, current: {})", MAX_MESSAGE_LENGTH, input_len),
                );
            });
            ui.separator();
        }

        // Input takes remaining space, button stays visible
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            const BUTTON_WIDTH: f32 = 55.0;
            let input_width = (ui.available_width() - BUTTON_WIDTH - 4.0).max(0.0);
            let text_edit = egui::TextEdit::singleline(&mut self.input_text);
            ui.add_sized([input_width, 22.0], text_edit);
            if !self.is_generating {
                if ui.button("Send").clicked() {
                    self.send_message();
                }
            } else {
                if ui.button("Stop").clicked() {
                    self.stop_generation();
                }
            }
        });
    }

    fn validate_input(&self, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("Message cannot be empty".to_string());
        }
        const MAX_MESSAGE_LENGTH: usize = 4000;
        if text.len() > MAX_MESSAGE_LENGTH {
            return Err(format!("Message too long (max {} characters)", MAX_MESSAGE_LENGTH));
        }
        Ok(())
    }

    fn send_message(&mut self) {
        let text = self.input_text.trim().to_string();
        if let Err(e) = self.validate_input(&text) {
            self.pending_error = Some(e);
            return;
        }

        self.input_text.clear();
        self.add_message("user", &text);

        // Transition to generating state
        self.is_generating = true;
        self.current_response.clear();
        self.status = AppStatus::Generating;

        // Cancel any existing streaming task
        if let Some(task) = self.streaming_task.take() {
            task.abort();
        }

        let text_clone = text.clone();
        let tx = self.pending_tx.as_ref().unwrap().clone();
        let streaming = self.streaming;

        // Extract client fields before spawning to avoid MutexGuard across await
        let (base_url, system_prompt, conversation, http_client, api_key) = {
            let c = self.client.lock().unwrap();
            (
                c.base_url.clone(),
                c.system_prompt.clone(),
                c.conversation.clone(),
                c.http_client.clone(),
                c.api_key.clone(),
            )
        };

        let handle = tokio::spawn(async move {
            if streaming {
                Self::do_streaming(
                    base_url,
                    system_prompt,
                    conversation,
                    http_client,
                    api_key,
                    text_clone,
                    tx,
                )
                .await;
            } else {
                Self::do_send_message(
                    base_url,
                    system_prompt,
                    conversation,
                    http_client,
                    api_key,
                    text_clone,
                    tx,
                )
                .await;
            }
        });

        self.streaming_task = Some(handle);
    }

    async fn do_streaming(
        base_url: String,
        system_prompt: String,
        conversation: Arc<Mutex<Vec<crate::client::Message>>>,
        http_client: reqwest::Client,
        api_key: Option<String>,
        text: String,
        tx: mpsc::Sender<AppEvent>,
    ) {
        let tx_clone = tx.clone();
        let conversation_clone = conversation.clone();
        let result = ChatClient::stream_message_with_usage(
            &base_url,
            &system_prompt,
            conversation,
            &http_client,
            api_key.as_deref(),
            &text,
            move |chunk| {
                let _ = tx_clone.send(AppEvent::StreamChunk {
                    content: chunk.clone(),
                });
                Ok(())
            },
        )
        .await;

        match result {
            Ok(usage) => {
                let content = {
                    let c = conversation_clone.lock().unwrap();
                    c.last().map(|m| m.content.clone())
                };
                if let Some(content) = content {
                    let _ = tx.send(AppEvent::StreamComplete { content, usage });
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::StreamError { error: e.to_string() });
            }
        }
    }

    async fn do_send_message(
        base_url: String,
        system_prompt: String,
        conversation: Arc<Mutex<Vec<crate::client::Message>>>,
        http_client: reqwest::Client,
        api_key: Option<String>,
        text: String,
        tx: mpsc::Sender<AppEvent>,
    ) {
        let result = ChatClient::send_message(
            &base_url,
            &system_prompt,
            conversation,
            &http_client,
            api_key.as_deref(),
            &text,
        )
        .await;

        let _ = tx.send(match result {
            Ok((content, usage)) => AppEvent::MessageResult { content, usage },
            Err(e) => AppEvent::MessageError { error: e.to_string() },
        });
    }

    fn stop_generation(&mut self) {
        if let Some(task) = self.streaming_task.take() {
            task.abort();
        }
        self.is_generating = false;
        self.current_response.clear();
        self.status = AppStatus::Ready;
    }

    fn process_pending_events(&mut self) {
        // Drain all pending events first, collecting results to avoid borrow issues
        let mut events: Vec<AppEvent> = Vec::new();
        while let Ok(event) = self.pending_rx.lock().unwrap().try_recv() {
            events.push(event);
        }

        for event in events {
            match event {
                AppEvent::MessageResult { content, usage } => {
                    self.add_message("assistant", &content);
                    self.is_generating = false;
                    self.status = AppStatus::Ready;
                    self.progress += 1.0;
                    let server_n_ctx = self.get_effective_n_ctx();
                    if let Some(u) = &usage {
                        self.token_count = u.total_tokens;
                        self.context_used = if server_n_ctx > 0 {
                            (u.total_tokens as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    } else {
                        // Fallback: estimate tokens from message content when server doesn't report usage
                        self.token_count = Self::estimate_token_count(&content);
                        self.context_used = if server_n_ctx > 0 {
                            (self.token_count as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    }
                }
                AppEvent::MessageError { error } => {
                    self.status = AppStatus::Error(error.clone());
                    self.pending_error = Some(format!("Message failed: {}", error));
                    self.is_generating = false;
                    self.current_response.clear();
                }
                AppEvent::StreamChunk { content } => {
                    self.current_response.push_str(&content);
                }
                AppEvent::StreamComplete { content, usage } => {
                    self.add_message("assistant", &content);
                    self.current_response.clear();
                    self.is_generating = false;
                    self.status = AppStatus::Ready;
                    self.progress += 1.0;
                    let server_n_ctx = self.get_effective_n_ctx();
                    if let Some(u) = &usage {
                        self.token_count = u.total_tokens;
                        self.context_used = if server_n_ctx > 0 {
                            (u.total_tokens as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    } else {
                        // Fallback: estimate tokens from message content when server doesn't report usage
                        self.token_count = Self::estimate_token_count(&content);
                        self.context_used = if server_n_ctx > 0 {
                            (self.token_count as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    }
                }
                AppEvent::StreamError { error } => {
                    self.status = AppStatus::Error(error.clone());
                    self.pending_error = Some(format!("Stream failed: {}", error));
                    self.is_generating = false;
                    self.current_response.clear();
                }
            }
        }
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.chat_display.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        });
        // Truncate if too many messages
        if self.chat_display.len() > self.max_display_messages {
            self.chat_display.drain(..self.chat_display.len() - self.max_display_messages);
        }
    }

    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let status_text = match &self.status {
                AppStatus::Stopped => "● Stopped".to_string(),
                AppStatus::Connecting => "● Connecting...".to_string(),
                AppStatus::Ready => "● Ready".to_string(),
                AppStatus::Generating => "● Generating...".to_string(),
                AppStatus::Error(e) => format!("● Error: {}", e),
            };
            let status_color = match &self.status {
                AppStatus::Stopped => egui::Color32::GRAY,
                AppStatus::Connecting => egui::Color32::BLUE,
                AppStatus::Ready => egui::Color32::GREEN,
                AppStatus::Generating => egui::Color32::BLUE,
                AppStatus::Error(_) => egui::Color32::RED,
            };
            ui.label(egui::RichText::new(&status_text).color(status_color));

            if self.streaming {
                ui.separator();
                ui.label("Streaming");
            }
            ui.separator();
            ui.label(format!("Messages: {}", self.chat_display.len()));
        });
    }

    /// Rough token count estimation: ~4 chars per token is a common rule of thumb
    fn estimate_token_count(text: &str) -> u32 {
        if text.is_empty() {
            0
        } else {
            (text.len() as f32 / 4.0).ceil() as u32
        }
    }

    /// Returns true when in remote connection mode.
    fn is_remote_mode(&self) -> bool {
        let cfg = self.config.lock().unwrap();
        let is_remote = cfg.connection_type == crate::config::ConnectionType::Remote;
        drop(cfg);
        is_remote
    }

    /// Returns the effective n_ctx for display and percentage calculations.
    /// Local mode: uses the configured n_ctx (we control the server process).
    /// Remote mode: uses the n_ctx fetched from the remote server's /props endpoint.
    fn get_effective_n_ctx(&self) -> u32 {
        if self.is_remote_mode() && self.remote_n_ctx > 0 {
            self.remote_n_ctx
        } else {
            self.server.get_n_ctx()
        }
    }

    fn draw_bottom_bar(&self, ui: &mut egui::Ui) {
        let n_ctx = self.get_effective_n_ctx();
        let n_gpu_layers = self.server.get_n_gpu_layers();
        let threads = self.server.get_threads();

        ui.horizontal(|ui| {
            ui.label("Tokens:");
            ui.label(self.token_count.to_string());
            ui.label(" | Context: ");
            ui.label(format!("{:.1}%", self.context_used));
            ui.separator();
            ui.label(format!("Ctx: {} | GPU: {} | Threads: {}", n_ctx, n_gpu_layers, threads));
        });
    }

    fn handle_error(&mut self, err: &str) {
        self.status = AppStatus::Error(err.to_string());
        self.pending_error = Some(err.to_string());
    }

    pub fn show_settings_dialog(
        &mut self,
        ctx: &egui::Context,
        server: &Arc<ServerManager>,
        client: &Arc<Mutex<ChatClient>>,
        config: &Arc<Mutex<Config>>,
    ) {
        if self.show_settings && self.settings_dialog.is_none() {
            let cfg = config.lock().unwrap();
            self.settings_dialog = Some(SettingsDialog::new(&cfg));
            drop(cfg);
        }
        if let Some(dialog) = self.settings_dialog.as_mut() {
            dialog.show_dialog(ctx, server, client, config, &mut self.show_settings);
            if !self.show_settings {
                self.settings_dialog = None;
            }
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // Save config on app close
        let mut cfg = self.config.lock().unwrap();
        cfg.streaming = self.streaming;
        // Save chat history to config for persistence
        cfg.chat_history = self.chat_display.iter().map(|m| ConfigChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        }).collect();
        if let Err(e) = cfg.save() {
            eprintln!("Failed to save config: {}", e);
        }
    }
}

impl eframe::App for ChatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint during streaming for real-time updates
        if self.is_generating {
            ctx.request_repaint();
        }

        // Fetch remote server props periodically (every ~5s while connected)
        if self.is_remote_mode() && self.remote_n_ctx == 0 {
            let config = self.config.clone();
            let remote_n_ctx = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let remote_n_ctx_clone = remote_n_ctx.clone();
            if self.remote_n_ctx_handle.is_none() {
                let handle = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        let url = {
                            let cfg = config.lock().unwrap();
                            cfg.remote_url.clone()
                        };
                        if url.is_empty() {
                            continue;
                        }
                        let props_url = format!("{}/props", url.trim_end_matches('/'));
                        if let Ok(resp) = reqwest::get(&props_url).await {
                            if resp.status().is_success() {
                                if let Ok(text) = resp.text().await {
                                    if let Ok(props) = serde_json::from_str::<serde_json::Value>(&text) {
                                        if let Some(n_ctx) = props
                                            .get("default_generation_settings")
                                            .and_then(|s| s.get("n_ctx"))
                                            .and_then(|v| v.as_u64())
                                        {
                                            remote_n_ctx_clone.store(n_ctx as u32, std::sync::atomic::Ordering::Relaxed);
                                            tracing::info!("Remote server n_ctx: {}", n_ctx);
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
                self.remote_n_ctx_handle = Some(handle);
                self.remote_n_ctx_arc = Some(remote_n_ctx);
            }
            // Read current value each frame
            if let Some(arc) = &self.remote_n_ctx_arc {
                self.remote_n_ctx = arc.load(std::sync::atomic::Ordering::Relaxed);
            }
        } else if let Some(arc) = &self.remote_n_ctx_arc {
            self.remote_n_ctx = arc.load(std::sync::atomic::Ordering::Relaxed);
        }

        // Process any pending async results first
        self.process_pending_events();
        // Show settings dialog
        let server = self.server.clone();
        let client = self.client.clone();
        let config = self.config.clone();
        self.show_settings_dialog(ctx, &server, &client, &config);
        // Draw main UI
        self.setup_ui(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.save(storage);
    }
}
