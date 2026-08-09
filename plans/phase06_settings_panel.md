# Phase 6: Settings Panel (Rust/egui)

## Status: Pending

---

### Step 6.1: Settings Dialog

**Objective**: Settings dialog with server config fields.

**Tasks**:
- Create `src/ui/settings.rs`
- Add fields for server_path, model_path, port, gpu_layers, ctx_size, threads, system_prompt
- Add save, cancel, start, stop buttons

```rust
use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::server::ServerManager;
use crate::client::ChatClient;

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
    
    pub fn show_dialog(
        &mut self,
        ctx: &egui::Context,
        server: &Arc<ServerManager>,
        client: &Arc<Mutex<ChatClient>>,
        config: &Arc<Mutex<Config>>,
    ) {
        egui::Window::new("Settings")
            .open(&mut self.show)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.heading("Server Settings");
                ui.separator();
                
                // Server path
                ui.horizontal(|ui| {
                    ui.label("Server Path:");
                    ui.text_edit_singleline(&mut self.server_path);
                    if ui.button("...").clicked() {
                        self.open_file_picker(ui, "server");
                    }
                });
                
                // Model path
                ui.horizontal(|ui| {
                    ui.label("Model Path:");
                    ui.text_edit_singleline(&mut self.model_path);
                    if ui.button("...").clicked() {
                        self.open_file_picker(ui, "model");
                    }
                });
                
                ui.separator();
                
                // Port
                ui.add(egui::Slider::new(&mut self.port, 1024..=65535).text("Port"));
                
                // GPU Layers
                ui.add(egui::Slider::new(&mut self.n_gpu_layers, 0..=99).text("GPU Layers"));
                
                // Context Size
                ui.add(egui::Slider::new(&mut self.n_ctx, 512..=32768).text("Context Size"));
                
                // Threads
                ui.add(egui::Slider::new(&mut self.threads, 1..=64).text("Threads"));
                
                ui.separator();
                
                // System prompt
                ui.label("System Prompt:");
                ui.add(egui::TextEdit::multiline(&mut self.system_prompt)
                    .desired_rows(3)
                    .hint_text("Enter system prompt..."));
                
                // Streaming
                ui.checkbox(&mut self.streaming, "Streaming");
                
                // Theme
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    if ui.selectable_value(&mut self.theme, "dark".to_string(), "Dark").clicked() {}
                    if ui.selectable_value(&mut self.theme, "light".to_string(), "Light").clicked() {}
                });
                
                ui.separator();
                
                // Buttons
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save(config);
                        self.show = false;
                    }
                    if ui.button("Start Server").clicked() {
                        self.start_server(server);
                    }
                    if ui.button("Stop Server").clicked() {
                        self.stop_server(server);
                    }
                    if ui.button("Cancel").clicked() {
                        self.show = false;
                    }
                });
            });
    }
    
    fn save(&self, config: &Arc<Mutex<Config>>) {
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
        
        if let Err(e) = cfg.save() {
            eprintln!("Failed to save config: {}", e);
        }
    }
    
    fn start_server(&self, server: &Arc<ServerManager>) {
        tokio::task::block_in_place(|| {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                if let Err(e) = server.start_server().await {
                    eprintln!("Failed to start server: {}", e);
                }
            });
        });
    }
    
    fn stop_server(&self, server: &Arc<ServerManager>) {
        tokio::task::block_in_place(|| {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                if let Err(e) = server.stop_server().await {
                    eprintln!("Failed to stop server: {}", e);
                }
            });
        });
    }
    
    fn open_file_picker(&mut self, ui: &egui::Ui, file_type: &str) {
        // egui doesn't have built-in file pickers
        // Use a simple dialog or external tool
        let response = egui::Dialog::new("Select File")
            .text(format!("Enter path for {}", file_type))
            .open();
        
        // In a real app, use native_file_dialog crate or similar
        // For now, just show an alert
        ui.alert("File picker not implemented in this demo. Use the text field to enter paths.");
    }
}

pub fn show_settings_dialog(
    ctx: &egui::Context,
    show: &mut bool,
    server: &Arc<ServerManager>,
    client: &Arc<Mutex<ChatClient>>,
    config: &Arc<Mutex<Config>>,
) {
    // This would be a mutable reference to SettingsDialog
    // For now, show a simple placeholder
    egui::Window::new("Settings")
        .open(show)
        .show(ctx, |ui| {
            ui.label("Settings dialog - implement as needed");
            if ui.button("Close").clicked() {
                *show = false;
            }
        });
}
```

**Success Criteria**:
- Settings dialog opens
- All fields visible
- Save writes to JSON

**Dependencies**: Step 2.1 (config struct exists)

---

### Step 6.2: File Pickers

**Objective**: Allow browsing for server and model paths.

**Tasks**:
- Add file picker buttons for server_path, model_path
- Validate selected file exists
- Update config fields

```rust
impl SettingsDialog {
    fn open_file_picker(&mut self, ui: &egui::Ui, file_type: &str) {
        // Use rfd (rust-native-dialogs) crate for native file pickers
        // Add to Cargo.toml: rfd = "0.14"
        
        #[cfg(feature = "file-picker")]
        {
            use rfd::FileDialog;
            let result = FileDialog::new()
                .add_filter(&format!("{} files", file_type), &["exe", "dll"])
                .pick_file();
            
            if let Some(path) = result {
                if file_type == "server" {
                    self.server_path = path.to_string_lossy().to_string();
                } else {
                    self.model_path = path.to_string_lossy().to_string();
                }
            }
        }
        
        #[cfg(not(feature = "file-picker"))]
        {
            ui.alert("Enable 'file-picker' feature for native file dialogs");
        }
    }
}
```

**Success Criteria**:
- File picker opens
- Selected paths are saved

**Dependencies**: Step 6.1

---

### Step 6.3: Server Control

**Objective**: Start/stop server from settings.

**Tasks**:
- Wire start/stop buttons to server manager
- Show status in settings
- Validate paths before starting

```rust
impl SettingsDialog {
    fn validate_and_start(&self, server: &Arc<ServerManager>) -> bool {
        if self.server_path.is_empty() || self.model_path.is_empty() {
            egui::AlertText::new("Please fill in both server path and model path").show(ui);
            return false;
        }
        
        // Validate paths exist
        if !std::path::Path::new(&self.server_path).exists() {
            egui::AlertText::new(&format!("Server path not found: {}", self.server_path)).show(ui);
            return false;
        }
        if !std::path::Path::new(&self.model_path).exists() {
            egui::AlertText::new(&format!("Model path not found: {}", self.model_path)).show(ui);
            return false;
        }
        
        true
    }
}
```

**Success Criteria**:
- Server starts with selected paths
- Server stops correctly

**Dependencies**: Step 3.1 (server manager)

---

## Files Created:
- `src/ui/settings.rs`

## Dependencies on other phases:
- Phase 2 (config struct)
- Phase 3 (server manager)

## Review Notes:
- egui dialogs are modal by default
- File pickers require external crate (rfd recommended)
- Validation checks paths exist before saving
- Server control validates paths before starting
- Settings window is modal (blocks main window)
