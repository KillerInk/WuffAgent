use eframe::egui;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::ChatClient;
use crate::config::{ChatMessage as ConfigChatMessage, Config};
use crate::server::ServerManager;
use crate::ui::settings::SettingsDialog;
use crate::ui::theme::Theme;
pub use crate::ui::state::ChatApp;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum AppStatus {
    #[default]
    Stopped,
    Connecting,
    Ready,
    Generating,
    Error(String),
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

pub enum AppEvent {
    MessageResult { content: String, usage: Option<crate::types::Usage> },
    MessageError { error: String },
    StreamChunk { content: String },
    StreamComplete { content: String, usage: Option<crate::types::Usage> },
    StreamError { error: String },
    ToolCallWarning { tool_name: String, message: String },
}

impl ChatApp {
    pub(super) fn get_tool_definitions(&self) -> Vec<crate::tools::ToolDefinition> {
        self.tool_manager.get_tool_definitions()
    }

    fn setup_ui(&mut self, ctx: &egui::Context) {
        // Session sidebar — draw before other panels so it sits on the left
        let switched_id: Option<String> = {
            if let Some(ref mut panel) = self.sessions.sessions_panel {
                panel.draw(ctx)
            } else {
                None
            }
        };
        
        if let Some(ref mut panel) = self.sessions.sessions_panel {
            panel.update_notification(ctx);
        }
        
        // Switch session if needed (handles New button and history selection)
        if let Some(id) = switched_id {
            self.switch_session(&id);
        }

        egui::TopBottomPanel::top("menu_bar").resizable(false).show(ctx, |ui| {
            ui.set_min_height(32.0);
            ui.set_max_height(36.0);
            
            let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
            ui.visuals_mut().panel_fill = theme.background;
            
            ui.horizontal(|ui| {
                // App title with accent color
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.visuals_mut().override_text_color = Some(theme.primary);
                ui.heading("WuffAgent");
                ui.visuals_mut().override_text_color = None;
                
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Theme toggle button
                    let theme_btn = egui::Button::new("◐")
                        .fill(theme.surface_light)
                        .rounding(4.0);
                    if ui.add(theme_btn).clicked() {
                        self.toggle_theme(ctx);
                    }
                    
                    // Settings button
                    let settings_btn = egui::Button::new("⚙")
                        .fill(theme.surface_light)
                        .rounding(4.0);
                    if ui.add(settings_btn).clicked() {
                        self.show_settings = true;
                    }
                });
            });
        });

        // Bottom panels stack upward, so bottom_bar must be declared first to be at the bottom
        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
            ui.visuals_mut().panel_fill = theme.surface;
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
            self.draw_status_bar(ui);
            ui.separator();
            self.draw_bottom_bar(ui);
        });

        // Input area is its own bottom panel, anchored above the status bar
        egui::TopBottomPanel::bottom("input_panel")
            .default_height(50.0)
            .resizable(false)
            .show(ctx, |ui| {
                let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
                ui.visuals_mut().panel_fill = theme.surface;
                self.draw_input_area(ui);
            });

        // Chat area fills all remaining space between top bar and input panel
        egui::CentralPanel::default().show(ctx, |ui| {
            let theme = Theme::from_name(&self.config.lock().unwrap().theme.clone());
            ui.visuals_mut().panel_fill = theme.background;
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

        // Apply custom theme colors
        let theme = Theme::from_name(&new_theme);
        theme.apply(ctx);
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
                    self.stop_streaming();
                    self.progress += 1.0;
                    let server_n_ctx = self.get_effective_n_ctx();
                    if let Some(u) = &usage {
                        self.chat.token_count = u.total_tokens;
                        self.chat.context_used = if server_n_ctx > 0 {
                            (u.total_tokens as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    } else {
                        // Fallback: estimate tokens from message content when server doesn't report usage
                        self.chat.token_count = Self::estimate_token_count(&content);
                        self.chat.context_used = if server_n_ctx > 0 {
                            (self.chat.token_count as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    }
                }
                AppEvent::MessageError { error } => {
                    self.chat.status = AppStatus::Error(error.clone());
                    self.chat.pending_error = Some(format!("Message failed: {}", error));
                    self.stop_streaming();
                }
                AppEvent::StreamChunk { content } => {
                    self.chat.current_response.push_str(&content);
                }
                AppEvent::StreamComplete { content, usage } => {
                    self.add_message("assistant", &content);
                    self.stop_streaming();
                    self.progress += 1.0;
                    let server_n_ctx = self.get_effective_n_ctx();
                    if let Some(u) = &usage {
                        self.chat.token_count = u.total_tokens;
                        self.chat.context_used = if server_n_ctx > 0 {
                            (u.total_tokens as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    } else {
                        // Fallback: estimate tokens from message content when server doesn't report usage
                        self.chat.token_count = Self::estimate_token_count(&content);
                        self.chat.context_used = if server_n_ctx > 0 {
                            (self.chat.token_count as f32 / server_n_ctx as f32) * 100.0
                        } else {
                            0.0
                        };
                    }
                }
                AppEvent::StreamError { error } => {
                    self.chat.status = AppStatus::Error(error.clone());
                    self.chat.pending_error = Some(format!("Stream failed: {}", error));
                    self.stop_streaming();
                }
                AppEvent::ToolCallWarning { tool_name, message } => {
                    tracing::warn!(tool = tool_name, message = %message, "Tool call warning");
                    // Show as a pending warning (similar to error but non-fatal)
                    self.chat.pending_error = Some(format!("[{}] {}", tool_name, message));
                }
            }
        }
    }

    pub(super) fn add_message(&mut self, role: &str, content: &str) {
        self.add_message_with_image(role, content, None);
    }

    pub(super) fn add_message_with_image(&mut self, role: &str, content: &str, image: Option<String>) {
        let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();
        self.chat.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: timestamp.clone(),
            image,
        });
        // Update the underlying session message with timestamp
        {
            let cl = self.client.lock().unwrap();
            let mut conv = cl.conversation().lock().unwrap();
            if let Some(last) = conv.last_mut() {
                if last.role == role && last.content == content {
                    last.timestamp = timestamp;
                }
            }
        }
        // Truncate if too many messages
        if self.chat.messages.len() > self.sessions.max_display_messages {
            self.chat.messages.drain(..self.chat.messages.len() - self.sessions.max_display_messages);
        }
        // Save session after adding message; retry any prior failed saves first
        let client = self.client.lock().unwrap();
        client.retry_pending_saves();
        drop(client);
        if let Err(e) = self.client.lock().unwrap().save_session() {
            self.sessions.save_failure_message = Some(format!("Save failed — will retry on next message"));
            eprintln!("Failed to save session: {}", e);
        } else {
            self.sessions.save_failure_message = None;
        }
    }

    fn switch_session(&mut self, session_id: &str) {
        // Retry any prior failed saves before switching
        self.client.lock().unwrap().retry_pending_saves();
        // Save current session before switching
        if let Err(e) = self.client.lock().unwrap().save_session() {
            self.sessions.save_failure_message = Some(format!("Save failed — will retry on next message"));
            eprintln!("Failed to save session before switch: {}", e);
        } else {
            self.sessions.save_failure_message = None;
        }

        let mut cl = self.client.lock().unwrap();
        // Update session_id before loading
        let session_dir = cl.session_dir().clone();
        cl.set_session(Some(session_id.to_string()), session_dir);
        // Always update chat_display, even when load_session returns None (new session)
        let loaded = cl.load_session();
        if let Some(session) = loaded {
            self.chat.messages = session.messages.iter().map(|m| ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                timestamp: m.timestamp.clone(),
                image: None,
            }).collect();
            cl.set_system_prompt(&session.system_prompt);
        } else {
            // New or empty session — clear the chat display
            self.chat.messages.clear();
        }
        drop(cl);
    }

    pub(super) fn start_streaming(&mut self) {
        self.chat.is_generating = true;
        self.chat.status = AppStatus::Generating;
        self.chat.current_response.clear();
    }

    pub(super) fn stop_streaming(&mut self) {
        self.chat.is_generating = false;
        self.chat.current_response.clear();
        self.chat.status = AppStatus::Ready;
    }

    pub fn show_settings_dialog(
        &mut self,
        ctx: &egui::Context,
        server: &Arc<ServerManager>,
        client: &Arc<Mutex<ChatClient>>,
        config: &Arc<Mutex<Config>>,
    ) {
        if self.show_settings && self.settings_dialog.is_none() {
            self.settings_dialog = Some(SettingsDialog::new(config));
        }
        if let Some(dialog) = self.settings_dialog.as_mut() {
            let closed = dialog.show(ctx, config);
            if closed {
                self.show_settings = false;
                self.settings_dialog = None;
            }
        }
    }

    /// Update the save-failure notification state each frame based on the client's flag.
    fn update_save_failure_notification(&mut self) {
        let client = self.client.lock().unwrap();
        if client.has_save_failure() && self.sessions.save_failure_message.is_none() {
            self.sessions.save_failure_message = Some("Save failed — will retry on next message".to_string());
        } else if !client.has_save_failure() {
            self.sessions.save_failure_message = None;
        }
        drop(client);
    }
}

impl eframe::App for ChatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint during streaming for real-time updates
        if self.chat.is_generating {
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
        // Update save-failure notification state
        self.update_save_failure_notification();
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
        cfg.streaming = self.chat.streaming;
        cfg.auto_scroll = self.chat.auto_scroll;
        cfg.chat_history = self.chat.messages.iter().map(|m| ConfigChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
        }).collect();
        // Note: images are not persisted in config chat_history (they're in session)
        if let Err(e) = cfg.save() {
            eprintln!("Failed to save config: {}", e);
        }
        // Save current session
        if let Err(e) = self.client.lock().unwrap().save_session() {
            eprintln!("Failed to save session: {}", e);
        }
    }
}
