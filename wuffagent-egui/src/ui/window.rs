use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use crate::config::{get_presets_path, PresetStore};
pub use crate::ui::state::ChatApp;

impl ChatApp {
    /// Shared config handle used by the settings/presets dialogs. Created when
    /// the settings dialog opens (as a clone of the live config) and dropped
    /// when it closes. Each frame the app syncs `self.config` from it so that
    /// Save (settings) and Load (presets) take effect in the running app.
    fn active_config_handle(&self) -> Option<Arc<Mutex<crate::config::Config>>> {
        self.settings_dialog
            .as_ref()
            .map(|d| d.config.clone())
            .or_else(|| self.presets_dialog.as_ref().map(|d| d.config.clone()))
    }

    pub fn show_settings_dialog(&mut self, ctx: &egui::Context) {
        if self.show_settings && self.settings_dialog.is_none() {
            let show_presets = Arc::new(Mutex::new(false));
            // Reuse the presets dialog's shared config if one is already open,
            // so both dialogs and the app stay in sync.
            let shared = self
                .presets_dialog
                .as_ref()
                .map(|d| d.config.clone())
                .unwrap_or_else(|| Arc::new(Mutex::new(self.config.clone())));
            self.settings_dialog =
                Some(super::settings::SettingsDialog::new_with_presets_flag(&shared, show_presets));
        }
        if let Some(dialog) = self.settings_dialog.as_mut() {
            let closed = dialog.show(ctx);
            if closed {
                self.show_settings = false;
                self.settings_dialog = None;
            }
        }
        self.sync_config_from_dialogs();
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
                            // Share the settings dialog's config handle so
                            // "Load" in presets and the app stay in sync.
                            let shared = sd.config.clone();
                            self.presets_dialog = Some(super::presets_dialog::PresetsDialog::new(
                                store,
                                &shared,
                            ));
                        }
                    }
                }
            }
        }

        if let Some(dialog) = self.presets_dialog.as_mut() {
            let closed = dialog.show(ctx);
            if closed {
                if let Err(e) = dialog.store.save(&get_presets_path()) {
                    eprintln!("Failed to save presets: {}", e);
                }
                self.presets_dialog = None;
            }
        }
        self.sync_config_from_dialogs();
    }

    /// Copy the shared dialog config back into `self.config` if any dialog is
    /// open. Dialogs mutate their (shared) config on Save/Load and persist it
    /// to disk; without this sync the running app would keep using stale values.
    fn sync_config_from_dialogs(&mut self) {
        if let Some(handle) = self.active_config_handle() {
            if let Ok(cfg) = handle.try_lock() {
                self.config = (*cfg).clone();
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
        //  - the merge task (engine_rx → AppEvent + tool_rx → AppEvent)
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
        // Update save failure notification state for the active session
        if let Some(id) = &self.selected_session_id {
            if let Some(runtime) = self.session_store.get(id) {
                if runtime.client.has_save_failure() {
                    // Could show a toast/notification here
                }
            }
        }
    }

    pub fn switch_session(&mut self, id: &str) {
        // Save the current session before switching
        if let Err(e) = self.save_session() {
            eprintln!("Failed to save session before switch: {}", e);
        }

        // Update the panel's selected_id so the UI reflects the switch immediately
        if let Some(ref mut panel) = self.sessions_panel {
            panel.select_session(id);
        }

        // Check if we already have a runtime for this session
        if self.session_store.contains_key(id) {
            // Update selected_session_id to point to the existing runtime
            self.selected_session_id = Some(id.to_string());
            return;
        }

        // Create a new runtime for this session
        let base_url = self.config.base_url();
        let mut session_client = crate::client::ChatClient::new(&base_url);
        session_client.set_api_key(self.config.remote_api_key.as_deref());
        let sessions_dir = self.config.sessions_dir.clone();
        session_client.set_session(Some(id.to_string()), sessions_dir.clone());
        session_client.set_reasoning_effort(self.config.reasoning_effort);
        session_client.set_max_messages(self.config.max_messages);
        session_client.set_n_ctx(self.config.n_ctx);
        if self.config.encryption_enabled {
            if let Some(key) = self.config.encryption_key() {
                session_client.set_encryption_key(Some(key));
            }
        }
        let _ = session_client.load_session();

        // Per-session engine: bound to this session's client so the agent chat
        // loop reads/writes an isolated conversation store (see the same
        // rationale in `state.rs` / `main.rs`).
        let session_engine = (*self.agent_engine).clone().with_client(session_client.clone());
        // Create pipeline (events flow through the shared pending_tx) and runtime
        let pipeline = crate::client::ChatPipeline::new(
            Arc::new(session_engine.clone()),
            self.pending_tx.as_ref().unwrap().lock().unwrap().clone(),
            self.config.reasoning_effort,
            id.to_string(),
        );
        let cancel_token = tokio_util::sync::CancellationToken::new();

        let runtime = crate::sessions::SessionRuntime::new(
            id.to_string(),
            // Try to get the session name from disk
            crate::sessions::load_session(&sessions_dir, id)
                .map(|s| s.name)
                .unwrap_or_else(|| format!("Session {}", id)),
            session_client,
            pipeline,
            session_engine,
            cancel_token,
        );

        self.session_store.insert(id.to_string(), runtime);
        self.selected_session_id = Some(id.to_string());

        // Populate the chat display from the conversation we just loaded
        // from disk so the history is visible immediately.
        if let Some(runtime) = self.session_store.get_mut(id) {
            let conv = runtime.client.conversation().clone();
            runtime.chat_state.reload_messages_from_client(&conv);
        }

        // Refresh the token gauge from the loaded conversation (exact char
        // counter, same units the trimmer uses).
        let n_ctx = self.get_effective_n_ctx();
        if let Some(runtime) = self.session_store.get_mut(id) {
            runtime.refresh_token_gauge(n_ctx);
        }
    }
}

impl eframe::App for ChatApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx();
        // Request repaint during streaming for real-time updates
        if self.selected_chat_state().map(|c| c.is_generating).unwrap_or(false) {
            ctx.request_repaint();
        }

        // Fetch the connected server's /props (n_ctx) in BOTH local and remote
        // mode. The server is the source of truth for the context limit: in
        // remote mode it is the only way to learn the limit, and in local mode
        // it catches a mismatch between the configured n_ctx and what the
        // server actually reports. The first fetch happens immediately so the
        // real context limit is available for the first chat turn; it retries
        // every 5s until the value arrives (e.g. a local server that is still
        // starting up).
        if self.remote_n_ctx_handle.is_none() {
            let config = self.config.clone();
            let server_n_ctx = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let server_n_ctx_clone = Arc::clone(&server_n_ctx);
            let handle = tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new());
                loop {
                    let url = config.base_url();
                    if url.is_empty() {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                    let props_url = format!("{}/props", url.trim_end_matches('/'));
                    let got_n_ctx = match client.get(&props_url).send().await {
                        Err(e) => {
                            tracing::warn!("props fetch failed ({}): {}", props_url, e);
                            None
                        }
                        Ok(resp) => {
                            if !resp.status().is_success() {
                                tracing::warn!(
                                    "props fetch returned HTTP {} for {}",
                                    resp.status(),
                                    props_url
                                );
                                None
                            } else {
                                match resp.text().await {
                                    Ok(text) => match crate::client::parse_props_n_ctx(&text) {
                                        Some(n_ctx) => {
                                            tracing::info!("Server n_ctx: {}", n_ctx);
                                            Some(n_ctx)
                                        }
                                        None => {
                                            tracing::warn!(
                                                "props response has no n_ctx (tried default_generation_settings.n_ctx and top-level n_ctx); body (first 500 chars): {}",
                                                &text.chars().take(500).collect::<String>()
                                            );
                                            None
                                        }
                                    },
                                    Err(e) => {
                                        tracing::warn!("props body read failed: {}", e);
                                        None
                                    }
                                }
                            }
                        }
                    };
                    if let Some(n_ctx) = got_n_ctx {
                        server_n_ctx_clone.store(n_ctx, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
            self.remote_n_ctx_handle = Some(handle);
            self.remote_n_ctx_arc = Some(server_n_ctx);
        }
        // Read current value each frame
        if let Some(arc) = &self.remote_n_ctx_arc {
            self.remote_n_ctx = arc.load(std::sync::atomic::Ordering::Relaxed);
        }

        // Process any pending async results first
        self.process_pending_events();
        // Defensive sweep: if a session is still flagged as generating but its
        // pipeline task is already done, a terminal event was lost (e.g. the
        // task was aborted by a new `start()` before it could emit
        // StreamComplete/StreamError, or it panicked). Clear the generating
        // state and commit any partial stream so the spinner can't get stuck.
        self.sweep_finished_pipelines();
        // Retry any pending session saves for the selected session
        if let Some(client) = self.active_client() {
            client.retry_pending_saves();
        }
        // Update save-failure notification state
        self.update_save_failure_notification();
        // Show settings dialog
        self.show_settings_dialog(ctx);
        // Show presets dialog (may be triggered from settings)
        self.show_presets_dialog(ctx);
        // Show agent config dialog
        self.show_agent_config_dialog(ctx);
        // Draw main UI into the root viewport ui (margins/background are
        // applied by the panels themselves, mirroring the old CentralPanel
        // fill behaviour).
        self.setup_ui(ui);
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