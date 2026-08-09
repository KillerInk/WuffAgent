use eframe::egui;
use std::sync::{Arc, Mutex};
use std::path::Path;

use crate::config::Config;
use crate::server::ServerManager;
use crate::client::ChatClient;

#[cfg(feature = "rfd")]
use rfd::FileDialog;

pub struct SettingsDialog {
    pub show: bool,
    server_path: String,
    model_path: String,
    port: u16,
    n_gpu_layers: i32,
    n_ctx: u32,
    threads: u32,
    system_prompt: String,
    streaming: bool,
    theme: String,
}

impl SettingsDialog {
    pub fn new(config: &Config) -> Self {
        Self {
            show: false,
            server_path: config.server_path.clone(),
            model_path: config.model_path.clone(),
            port: config.port,
            n_gpu_layers: config.n_gpu_layers,
            n_ctx: config.n_ctx,
            threads: config.threads,
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
    ) {
        // Capture fields into local variables to avoid borrowing `self` inside the closure
        let mut server_path = self.server_path.clone();
        let mut model_path = self.model_path.clone();
        let mut port = self.port;
        let mut n_gpu_layers = self.n_gpu_layers;
        let mut n_ctx = self.n_ctx;
        let mut threads = self.threads;
        let mut system_prompt = self.system_prompt.clone();
        let mut streaming = self.streaming;
        let mut theme = self.theme.clone();

        egui::Window::new("Settings")
            .vscroll(true)
            .show(ctx, |ui| {
                ui.heading("Server Settings");
                ui.separator();

                // Server path
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

                // Server status display
                ui.horizontal(|ui| {
                    ui.label("Server Status:");
                    let running = server.is_running();
                    let status_text = if running { "Running" } else { "Stopped" };
                    let status_color = if running { egui::Color32::GREEN } else { egui::Color32::RED };
                    ui.label(egui::RichText::new(status_text).color(status_color));
                });

                // Buttons
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        // Save config
                        let mut cfg = config.lock().unwrap();
                        cfg.server_path = server_path.clone();
                        cfg.model_path = model_path.clone();
                        cfg.port = port;
                        cfg.n_gpu_layers = n_gpu_layers;
                        cfg.n_ctx = n_ctx;
                        cfg.threads = threads;
                        cfg.system_prompt = system_prompt.clone();
                        cfg.streaming = streaming;
                        cfg.theme = theme.clone();
                        if let Err(e) = cfg.save() {
                            eprintln!("Failed to save config: {}", e);
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
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
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                    if ui.button("Cancel").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });

        // Sync back
        self.server_path = server_path;
        self.model_path = model_path;
        self.port = port;
        self.n_gpu_layers = n_gpu_layers;
        self.n_ctx = n_ctx;
        self.threads = threads;
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

pub fn show_settings_dialog(
    ctx: &egui::Context,
    show: &mut bool,
    server: &Arc<ServerManager>,
    client: &Arc<Mutex<ChatClient>>,
    config: &Arc<Mutex<Config>>,
) {
    if !*show {
        return;
    }

    // Read current config values
    let cfg = config.lock().unwrap();
    let mut dialog = SettingsDialog::new(&cfg);
    drop(cfg);

    dialog.show_dialog(ctx, server, client, config);
}
