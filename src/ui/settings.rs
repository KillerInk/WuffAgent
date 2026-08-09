use eframe::egui;
use std::sync::{Arc, Mutex};
use std::path::Path;

use crate::config::{Config, ConnectionType};
use crate::server::ServerManager;
use crate::client::ChatClient;
use crate::sessions;

/// Format a chrono DateTime<Utc> to a human-readable relative time string.
fn relative_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*dt);
    let seconds = duration.num_seconds().abs();
    if seconds < 10 {
        "just now".to_string()
    } else if seconds < 60 {
        format!("{} sec ago", seconds)
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        format!("{} min ago", minutes)
    } else if seconds < 86400 {
        let hours = seconds / 3600;
        format!("{} hours ago", hours)
    } else {
        let days = seconds / 86400;
        format!("{} days ago", days)
    }
}

#[cfg(feature = "rfd")]
use rfd::FileDialog;

pub struct SettingsDialog {
    connection_type: ConnectionType,
    remote_url: String,
    remote_api_key: String,
    server_path: String,
    model_path: String,
    port: u16,
    n_gpu_layers: i32,
    n_ctx: u32,
    threads: u32,
    max_messages: usize,
    system_prompt: String,
    streaming: bool,
    theme: String,
}

impl SettingsDialog {
    pub fn new(config: &Config) -> Self {
        Self {
            connection_type: config.connection_type.clone(),
            remote_url: config.remote_url.clone(),
            remote_api_key: config.remote_api_key.clone().unwrap_or_default(),
            server_path: config.server_path.clone(),
            model_path: config.model_path.clone(),
            port: config.port,
            n_gpu_layers: config.n_gpu_layers,
            n_ctx: config.n_ctx,
            threads: config.threads,
            max_messages: config.max_messages,
            system_prompt: config.system_prompt.clone(),
            streaming: config.streaming,
            theme: config.theme.clone(),
        }
    }

    fn show_error(ui: &mut egui::Ui, msg: &str) {
        ui.colored_label(egui::Color32::RED, msg);
    }

    pub fn show_dialog(
        &mut self,
        ctx: &egui::Context,
        server: &Arc<ServerManager>,
        client: &Arc<Mutex<ChatClient>>,
        config: &Arc<Mutex<Config>>,
        open: &mut bool,
    ) {
        // Capture fields into local variables to avoid borrowing `self` inside the closure
        let mut connection_type = self.connection_type.clone();
        let mut remote_url = self.remote_url.clone();
        let mut remote_api_key = self.remote_api_key.clone();
        let mut server_path = self.server_path.clone();
        let mut model_path = self.model_path.clone();
        let mut port = self.port;
        let mut n_gpu_layers = self.n_gpu_layers;
        let mut n_ctx = self.n_ctx;
        let mut threads = self.threads;
        let mut max_messages = self.max_messages;
        let mut system_prompt = self.system_prompt.clone();
        let mut streaming = self.streaming;
        let mut theme = self.theme.clone();

        egui::Window::new("Settings")
            .vscroll(true)
            .show(ctx, |ui| {
                ui.heading("Connection Settings");
                ui.separator();

                // Connection type selector
                ui.horizontal(|ui| {
                    ui.label("Connection:");
                    if ui.selectable_label(connection_type == ConnectionType::Local, "Local").clicked() {
                        connection_type = ConnectionType::Local;
                    }
                    if ui.selectable_label(connection_type == ConnectionType::Remote, "Remote").clicked() {
                        connection_type = ConnectionType::Remote;
                    }
                });

                ui.separator();

                match connection_type {
                    ConnectionType::Local => {
                        // Local server path
                        ui.horizontal(|ui| {
                            ui.label("Server Path:");
                            ui.text_edit_singleline(&mut server_path);
                            #[cfg(feature = "rfd")]
                            if ui.button("...").clicked() {
                                // open file picker (handled after UI)
                            }
                        });

                        // Model path
                        ui.horizontal(|ui| {
                            ui.label("Model Path:");
                            ui.text_edit_singleline(&mut model_path);
                            #[cfg(feature = "rfd")]
                            if ui.button("...").clicked() {
                                // open file picker (handled after UI)
                            }
                        });

                        ui.separator();

                        // Port
                        ui.add(egui::Slider::new(&mut port, 1024..=65535).text("Port"));

                        // GPU Layers
                        ui.add(egui::Slider::new(&mut n_gpu_layers, 0..=99).text("GPU Layers"));

                        // Context Size
                        ui.add(egui::Slider::new(&mut n_ctx, 512..=32768).text("Context Size"));

                        // Threads
                        ui.add(egui::Slider::new(&mut threads, 1..=64).text("Threads"));

                        // Max Messages
                        ui.add(egui::Slider::new(&mut max_messages, 10..=1000).text("Max Messages"));
                    }
                    ConnectionType::Remote => {
                        // Remote URL
                        ui.horizontal(|ui| {
                            ui.label("Remote URL:");
                            ui.text_edit_singleline(&mut remote_url);
                        });
                        ui.label(egui::RichText::new("Example: http://192.168.1.100:8080").color(egui::Color32::DARK_GRAY));

                        // API key (optional)
                        ui.horizontal(|ui| {
                            ui.label("API Key (optional):");
                            ui.text_edit_singleline(&mut remote_api_key);
                        });
                        ui.label(egui::RichText::new("Leave empty if no authentication required").color(egui::Color32::DARK_GRAY));
                    }
                }

                ui.separator();

                // System prompt
                ui.label("System Prompt:");
                ui.add(egui::TextEdit::multiline(&mut system_prompt)
                    .desired_rows(3)
                    .hint_text("Enter system prompt..."));

                // Streaming
                ui.checkbox(&mut streaming, "Streaming");

                // Theme
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    if ui.selectable_value(&mut theme, "dark".to_string(), "Dark").clicked() {}
                    if ui.selectable_value(&mut theme, "light".to_string(), "Light").clicked() {}
                });

                ui.separator();

                // Storage Info section
                ui.heading("Storage Info");
                ui.separator();
                {
                    let cfg = config.lock().unwrap();
                    let sessions_dir = cfg.sessions_dir().clone();
                    drop(cfg);
                    let (count, total_size) = sessions::session_stats(&sessions_dir);
                    ui.label(format!("Sessions directory: {}", sessions_dir.display()));
                    ui.label(format!("Sessions: {}", count));
                    let size_str = if total_size < 1024 {
                        format!("{} B", total_size)
                    } else if total_size < 1024 * 1024 {
                        format!("{:.1} KB", total_size as f64 / 1024.0)
                    } else {
                        format!("{:.1} MB", total_size as f64 / (1024.0 * 1024.0))
                    };
                    ui.label(format!("Total size: {}", size_str));

                    // Backup status info
                    if let Ok(backups) = sessions::count_backups(&sessions_dir) {
                        ui.label(format!("Backup files: {}", backups));
                        if let Some(last_backup) = sessions::last_backup_time(&sessions_dir) {
                            let rel = relative_time(&last_backup);
                            ui.label(format!("Last backup: {}", rel));
                        }
                    }

                    if ui.button("Open folder").clicked() {
                        let _ = std::process::Command::new("explorer").arg(&sessions_dir).spawn();
                    }
                }

                ui.separator();

                // Server status display
                ui.horizontal(|ui| {
                    ui.label("Connection Status:");
                    if connection_type == ConnectionType::Remote {
                        let url_text = if remote_url.is_empty() {
                            "Not configured".to_string()
                        } else {
                            format!("Remote ({})", remote_url)
                        };
                        let status_color = if remote_url.is_empty() {
                            egui::Color32::GRAY
                        } else {
                            egui::Color32::GREEN
                        };
                        ui.label(egui::RichText::new(url_text).color(status_color));
                    } else {
                        let running = server.is_running();
                        let status_text = if running { "Running" } else { "Stopped" };
                        let status_color = if running { egui::Color32::GREEN } else { egui::Color32::RED };
                        ui.label(egui::RichText::new(status_text).color(status_color));
                    }
                });

                // Buttons
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        // Save config
                        let mut cfg = config.lock().unwrap();
                        cfg.connection_type = connection_type.clone();
                        cfg.remote_url = remote_url.clone();
                        cfg.remote_api_key = if remote_api_key.is_empty() {
                            None
                        } else {
                            Some(remote_api_key.clone())
                        };
                        cfg.server_path = server_path.clone();
                        cfg.model_path = model_path.clone();
                        cfg.port = port;
                        cfg.n_gpu_layers = n_gpu_layers;
                        cfg.n_ctx = n_ctx;
                        cfg.threads = threads;
                        cfg.max_messages = max_messages;
                        cfg.system_prompt = system_prompt.clone();
                        cfg.streaming = streaming;
                        cfg.theme = theme.clone();
                        if let Err(e) = cfg.save() {
                            ui.label(egui::RichText::new(format!("Failed to save config: {}", e)).color(egui::Color32::RED));
                        } else {
                            // Update client URL and API key
                            let mut cl = client.lock().unwrap();
                            let new_url = cfg.base_url();
                            cl.set_url(&new_url);
                            cl.set_api_key(cfg.remote_api_key.as_deref());
                            cl.set_system_prompt(&system_prompt);
                            cl.set_max_messages(max_messages);
                            drop(cl);
                            *open = false;
                        }
                    }
                    if connection_type == ConnectionType::Local {
                        if ui.button("Start Server").clicked() {
                            // Validate
                            if server_path.is_empty() || model_path.is_empty() {
                                ui.label(egui::RichText::new("Please fill in both server path and model path").color(egui::Color32::RED));
                            } else if !Path::new(&server_path).exists() {
                                ui.label(egui::RichText::new(format!("Server path not found: {}", server_path)).color(egui::Color32::RED));
                            } else if !Path::new(&model_path).exists() {
                                ui.label(egui::RichText::new(format!("Model path not found: {}", model_path)).color(egui::Color32::RED));
                            } else {
                                // Stop existing
                                let server_clone = server.clone();
                                tokio::task::block_in_place(|| {
                                    let rt = tokio::runtime::Handle::current();
                                    rt.block_on(async {
                                        if let Err(e) = server_clone.stop_server().await {
                                            eprintln!("Failed to stop server: {}", e);
                                        }
                                    });
                                });

                                // Start new
                                let server_clone = server.clone();
                                let config_clone = config.clone();
                                let client_clone = client.clone();
                                let port_val = port;
                                let n_gpu = n_gpu_layers;
                                let n_ctx_val = n_ctx;
                                let threads_val = threads;
                                let sp = system_prompt.clone();
                                let server_path_val = server_path.clone();
                                let model_path_val = model_path.clone();

                                let start_result = tokio::task::block_in_place(|| {
                                    let rt = tokio::runtime::Handle::current();
                                    rt.block_on(async {
                                        server_clone.start_server_with_paths(
                                            &server_path_val,
                                            &model_path_val,
                                            port_val,
                                            n_gpu,
                                            n_ctx_val,
                                            threads_val,
                                        ).await
                                    })
                                });

                                match start_result {
                                    Ok(_) => {
                                        // Save updated config
                                        let mut cfg = config_clone.lock().unwrap();
                                        cfg.server_path = server_path.clone();
                                        cfg.model_path = model_path.clone();
                                        cfg.port = port_val;
                                        cfg.n_gpu_layers = n_gpu;
                                        cfg.n_ctx = n_ctx_val;
                                        cfg.threads = threads_val;
                                        cfg.max_messages = max_messages;
                                        cfg.streaming = streaming;
                                        cfg.system_prompt = sp.clone();
                                        if let Err(e) = cfg.save() {
                                            eprintln!("Failed to save updated config: {}", e);
                                        }
                                        // Update client URL
                                        let new_url = format!("http://127.0.0.1:{}", port_val);
                                        let mut cl = client_clone.lock().unwrap();
                                        cl.set_url(&new_url);
                                        cl.set_system_prompt(&sp);
                                        drop(cl);
                                        *open = false;
                                    }
                                    Err(e) => {
                                        ui.label(egui::RichText::new(format!("Failed to start server: {}", e)).color(egui::Color32::RED));
                                    }
                                }
                            }
                        }
                        if ui.button("Stop Server").clicked() {
                            let server_clone = server.clone();
                            tokio::task::block_in_place(|| {
                                let rt = tokio::runtime::Handle::current();
                                rt.block_on(async {
                                    if let Err(e) = server_clone.stop_server().await {
                                        eprintln!("Failed to stop server: {}", e);
                                    }
                                });
                            });
                        }
                    }
                    if ui.button("Close").clicked() {
                        // Save on close to persist any unsaved changes
                        let mut cfg = config.lock().unwrap();
                        cfg.connection_type = connection_type.clone();
                        cfg.remote_url = remote_url.clone();
                        cfg.remote_api_key = if remote_api_key.is_empty() {
                            None
                        } else {
                            Some(remote_api_key.clone())
                        };
                        cfg.server_path = server_path.clone();
                        cfg.model_path = model_path.clone();
                        cfg.port = port;
                        cfg.n_gpu_layers = n_gpu_layers;
                        cfg.n_ctx = n_ctx;
                        cfg.threads = threads;
                        cfg.max_messages = max_messages;
                        cfg.system_prompt = system_prompt.clone();
                        cfg.streaming = streaming;
                        cfg.theme = theme.clone();
                        if let Err(e) = cfg.save() {
                            eprintln!("Failed to save config: {}", e);
                        }
                        *open = false;
                    }
                });
            });
        // Write back any changes
        self.connection_type = connection_type;
        self.remote_url = remote_url;
        self.remote_api_key = remote_api_key;
        self.server_path = server_path;
        self.model_path = model_path;
        self.port = port;
        self.n_gpu_layers = n_gpu_layers;
        self.n_ctx = n_ctx;
        self.threads = threads;
        self.max_messages = max_messages;
        self.system_prompt = system_prompt;
        self.streaming = streaming;
        self.theme = theme;
    }
}

#[cfg(feature = "rfd")]
fn open_file_picker(file_type: &str) -> Option<String> {
    let result = FileDialog::new()
        .add_filter("Executable files", &["exe", "dll"])
        .add_filter("Model files", &["gguf", "bin"])
        .pick_file();

    result.map(|p| p.to_string_lossy().to_string())
}

#[cfg(not(feature = "rfd"))]
fn open_file_picker(_file_type: &str) -> Option<String> {
    None
}
