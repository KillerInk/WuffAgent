use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use crate::config::{get_presets_path, PresetStore};
pub use crate::ui::state::ChatApp;

impl ChatApp {
    pub fn show_settings_dialog(&mut self, ctx: &egui::Context) {
        if self.show_settings && self.settings_dialog.is_none() {
            let show_presets = Arc::new(Mutex::new(false));
            self.settings_dialog =
                Some(super::settings::SettingsDialog::new_with_presets_flag(&Arc::new(Mutex::new(self.config.clone())), show_presets));
        }
        if let Some(dialog) = self.settings_dialog.as_mut() {
            let closed = dialog.show(ctx, &Arc::new(Mutex::new(self.config.clone())));
            if closed {
                self.show_settings = false;
                self.settings_dialog = None;
            }
        }
    }

    pub fn show_presets_dialog(&mut self, ctx: &egui::Context) {
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
            let closed = dialog.show(ctx, &Arc::new(Mutex::new(self.config.clone())));
            if closed {
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

    pub fn show_agent_config_dialog(&mut self, ctx: &egui::Context) {
        let agents_dir = self.config.file_path.parent()
            .map(|p| p.join("agents"))
            .unwrap_or_else(|| PathBuf::from("agents"));
        if self.show_agent_config && self.agent_config_dialog.is_none() {
            let mut agent_manager = crate::agents::config::AgentManager::new(agents_dir.clone());
            // Scan project-level agents dirs for discovery (same as get_agent_names)
            if let Ok(cwd) = std::env::current_dir() {
                agent_manager.add_search_dir(cwd.join("agents"));
            }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    agent_manager.add_search_dir(exe_dir.join("agents"));
                }
            }
            self.agent_config_dialog =
                Some(super::agent_config::AgentConfigDialog::new(Arc::new(Mutex::new(agent_manager)), &self.tool_manager));
        }
        if let Some(dialog) = self.agent_config_dialog.as_mut() {
            let mut agent_manager = crate::agents::config::AgentManager::new(agents_dir.clone());
            if let Ok(cwd) = std::env::current_dir() {
                agent_manager.add_search_dir(cwd.join("agents"));
            }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    agent_manager.add_search_dir(exe_dir.join("agents"));
                }
            }
            let closed = dialog.show(ctx, &Arc::new(Mutex::new(agent_manager)));
            if closed {
                self.show_agent_config = false;
                self.agent_config_dialog = None;
            }
        }
    }

    pub fn process_pending_events(&mut self) {
        // Drain all buffered events from the channel and handle them.
        // Events are produced by:
        //  - the merge task (engine_rx â†’ AppEvent + tool_rx â†’ AppEvent)
        //  - any direct AppEvent sends from the client (tool calls)
        // Take the receiver out of the Option to avoid borrowing self mutably
        // while also calling self.handle_event().
        if let Some(rx) = self.pending_rx.take() {
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            // Put the receiver back
            self.pending_rx = Some(rx);
            for event in events {
                self.handle_event(event);
            }
        }
    }

    pub fn update_save_failure_notification(&mut self) {
        // Update save failure notification state
        if self.client.has_save_failure() {
            // Could show a toast/notification here
        }
    }

    pub fn switch_session(&mut self, id: &str) {
        // Save the current session before switching
        if let Err(e) = self.save_session() {
            eprintln!("Failed to save session before switch: {}", e);
        }
        // Switch to a different session. Must call on the real client:
        // `clear_session` sets `session_id` on the struct itself (not the
        // shared Arc), so calling it on a clone would leave the stale id
        // and resurrect the previous session on the next save.
        self.client.clear_session();
        self.chat.messages.clear();
        // Update the panel's selected_id so the UI reflects the switch immediately
        if let Some(ref mut panel) = self.sessions.sessions_panel {
            panel.select_session(id);
        }
        // Reload the selected session
        let session_dir = self.config.sessions_dir.clone();
        if let Some(session) = crate::sessions::load_session(&session_dir, id) {
            let mut conv = self.client.conversation().lock().unwrap();
            *conv = session.messages.clone();
            drop(conv);
            self.client.set_session(Some(id.to_string()), session_dir.clone());
            self.client.load_session();
            // Populate the UI display with the loaded session messages.
            // Derive message kind from legacy conventions (tool role, ðŸ'­ prefix).
            self.chat.messages = session.messages.iter().map(|m| {
                let (content, kind) = if m.role == "tool" {
                    (m.content.clone(), crate::types::MessageKind::Tool)
                } else if let Some(t) = m.content.strip_prefix("ðŸ'­ ") {
                    (t.to_string(), crate::types::MessageKind::Thinking)
                } else {
                    (m.content.clone(), crate::types::MessageKind::Normal)
                };
                crate::types::ChatMessage {
                    kind,
                    role: m.role.clone(),
                    content,
                    timestamp: m.timestamp.clone(),
                    image: None,
                }
            }).collect();
            // Estimate token count from loaded session messages
            let total_chars: usize = session.messages.iter().map(|m| m.content.len()).sum();
            self.chat.token_count = (total_chars as f32 / 4.0).ceil() as usize;
            let n_ctx = self.get_effective_n_ctx();
            if n_ctx > 0 {
                self.chat.context_used = self.chat.token_count as f32 / n_ctx as f32 * 100.0;
            }
        }

        // Also sync the agent engine's session so subsequent agent calls
        // load/save against the correct agent session directory.
        let agent_sid = self.agent_engine.agent_session_id().map(|s| s.to_string());
        if let Some(agent_name) = &agent_sid {
            let agent_dir = session_dir.join("agents").join(agent_name);
            if Arc::get_mut(&mut self.agent_engine).is_some() {
                if let Some(ref mut engine) = Arc::get_mut(&mut self.agent_engine) {
                    engine.set_agent_session(Some(agent_name.clone()), agent_dir);
                }
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
                        let url = config.remote_url.clone();
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
        // Retry any pending session saves
        self.client.clone().retry_pending_saves();
        // Update save-failure notification state
        self.update_save_failure_notification();
        // Show settings dialog
        self.show_settings_dialog(ctx);
        // Show presets dialog (may be triggered from settings)
        self.show_presets_dialog(ctx);
        // Show agent config dialog
        self.show_agent_config_dialog(ctx);
        // Draw main UI
        self.setup_ui(ctx);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        // Save config via centralized method
        if let Err(e) = self.save_config() {
            eprintln!("Failed to save config: {}", e);
        }
        // Save current session via centralized method
        if let Err(e) = self.save_session() {
            eprintln!("Failed to save session: {}", e);
        }
    }
}
