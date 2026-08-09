use eframe::egui;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::ChatClient;
use crate::config::{ChatMessage as ConfigChatMessage, Config};
use crate::server::ServerManager;
use crate::tools::ToolManager;
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
    MessageResult { content: String, usage: Option<crate::types::Usage> },
    MessageError { error: String },
    StreamChunk { content: String },
    StreamComplete { content: String, usage: Option<crate::types::Usage> },
    StreamError { error: String },
}

pub struct ChatApp {
    pub(super) server: Arc<ServerManager>,
    pub(super) client: Arc<Mutex<ChatClient>>,
    pub(super) config: Arc<Mutex<Config>>,
    pub(super) tool_manager: Arc<ToolManager>,
    pub(super) pending_tx: Option<mpsc::Sender<AppEvent>>,
    pub(super) pending_rx: Mutex<mpsc::Receiver<AppEvent>>,

    // UI state
    pub(super) chat_display: Vec<ChatMessage>,
    pub(super) input_text: String,
    pub(super) is_generating: bool,
    pub(super) status: AppStatus,
    pub(super) streaming: bool,
    pub(super) show_settings: bool,
    pub(super) settings_dialog: Option<SettingsDialog>,
    pub(super) progress: f32,
    pub(super) pending_error: Option<String>,

    // For streaming
    pub(super) current_response: String,
    pub(super) streaming_task: Option<JoinHandle<()>>,

    // Stats for bottom bar
    pub(super) token_count: u32,
    pub(super) context_used: f32,

    // Remote server n_ctx (fetched from /props), 0 = not yet fetched
    pub(super) remote_n_ctx: u32,

    // Shared Arc for the background fetch task to update
    pub(super) remote_n_ctx_arc: Option<Arc<std::sync::atomic::AtomicU32>>,

    // Shared handle for background remote n_ctx fetch task
    pub(super) remote_n_ctx_handle: Option<JoinHandle<()>>,

    // Max messages to keep in display (truncate for context window)
    pub(super) max_display_messages: usize,

    // Session sidebar
    pub(super) sessions_panel: Option<super::sessions_panel::SessionsPanel>,
}

impl ChatApp {
    pub fn new(
        server: Arc<ServerManager>,
        client: Arc<Mutex<ChatClient>>,
        config: Arc<Mutex<Config>>,
        tool_manager: Arc<ToolManager>,
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
            config: config.clone(),
            tool_manager,
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
            sessions_panel: Some(super::sessions_panel::SessionsPanel::new(&config.clone())),
        }
    }

    fn get_tool_definitions(&self) -> Vec<crate::tools::ToolDefinition> {
        self.tool_manager.get_tool_definitions()
    }

    fn setup_ui(&mut self, ctx: &egui::Context) {
        // Session sidebar — draw before other panels so it sits on the left
        if let Some(ref mut panel) = self.sessions_panel {
            if let Some(new_id) = panel.draw(ctx) {
                self.switch_session(&new_id);
            }
            if panel.clear_action {
                let mut cl = self.client.lock().unwrap();
                cl.clear_session_messages();
                panel.clear_action = false;
            }
        }

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
            self.draw_chat_area(ui);
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

    pub(super) fn add_message(&mut self, role: &str, content: &str) {
        self.chat_display.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        });
        // Truncate if too many messages
        if self.chat_display.len() > self.max_display_messages {
            self.chat_display.drain(..self.chat_display.len() - self.max_display_messages);
        }
    }

    fn switch_session(&mut self, _session_id: &str) {
        let mut cl = self.client.lock().unwrap();
        if let Some(session) = cl.load_session() {
            self.chat_display = session.messages.iter().map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            }).collect();
            cl.set_system_prompt(&session.system_prompt);
        }
        drop(cl);
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

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // Save config (existing)
        let mut cfg = self.config.lock().unwrap();
        cfg.streaming = self.streaming;
        cfg.chat_history = self.chat_display.iter().map(|m| ConfigChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        }).collect();
        if let Err(e) = cfg.save() {
            eprintln!("Failed to save config: {}", e);
        }
        // Save current session
        if let Err(e) = self.client.lock().unwrap().save_session() {
            eprintln!("Failed to save session: {}", e);
        }
    }
}
