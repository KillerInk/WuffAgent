use std::sync::{Arc, Mutex};

use crate::config::{ChatMessage as ConfigChatMessage, Config, get_presets_path, PresetStore};
pub use crate::ui::state::ChatApp;

impl ChatApp {
    pub fn show_settings_dialog(
        &mut self,
        ctx: &egui::Context,
        _server: &Arc<crate::server::ServerManager>,
        _client: &Arc<Mutex<crate::client::ChatClient>>,
        config: &Arc<Mutex<Config>>,
    ) {
        if self.show_settings && self.settings_dialog.is_none() {
            // Pass a shared flag so settings can signal us to open presets
            let show_presets = Arc::new(Mutex::new(false));
            self.settings_dialog =
                Some(super::settings::SettingsDialog::new_with_presets_flag(config, show_presets));
        }
        if let Some(dialog) = self.settings_dialog.as_mut() {
            let closed = dialog.show(ctx, config);
            if closed {
                self.show_settings = false;
                self.settings_dialog = None;
            }
        }
    }

    pub fn show_presets_dialog(
        &mut self,
        ctx: &egui::Context,
        config: &Arc<Mutex<Config>>,
    ) {
        // Check if settings dialog signaled us to open presets via shared flag
        if let Some(ref sd) = self.settings_dialog {
            if let Ok(flag) = sd.show_presets.lock() {
                if *flag {
                    drop(flag);
                    if let Ok(mut f) = sd.show_presets.lock() {
                        *f = false;
                    }
                    if self.presets_dialog.is_none() {
                        if let Ok(store) = PresetStore::load(&get_presets_path()) {
                            self.presets_dialog =
                                Some(super::presets_dialog::PresetsDialog::new(store));
                        }
                    }
                }
            }
        }

        if let Some(dialog) = self.presets_dialog.as_mut() {
            let closed = dialog.show(ctx, config);
            if closed {
                // Persist store to disk before closing
                if let Ok(path) = std::env::current_exe() {
                    if let Some(dir) = path.parent() {
                        let presets_path = dir.join("presets.json");
                        if let Err(e) = dialog.store.save(&presets_path) {
                            eprintln!("Failed to save presets: {}", e);
                        }
                    }
                }
                self.presets_dialog = None;
            }
        }
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
        // Show presets dialog (may be triggered from settings)
        self.show_presets_dialog(ctx, &config);
        // Draw main UI
        self.setup_ui(ctx);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // Save config (existing)
        let mut cfg = self.config.lock().unwrap();
        cfg.streaming = self.chat.streaming;
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
