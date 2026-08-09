pub mod client;
pub mod config;
pub mod server;
pub mod ui;

use std::sync::{Arc, Mutex};

use client::ChatClient;
use config::Config;
use eframe::egui;
use server::ServerManager;
use ui::window::ChatApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };

    // Load config
    let config_path = config::get_config_path();
    let config = match Config::load(&config_path) {
        Ok(cfg) => Arc::new(Mutex::new(cfg)),
        Err(e) => {
            eprintln!("Failed to load config: {}, using defaults", e);
            let default_cfg = Config::default();
            Arc::new(Mutex::new(default_cfg))
        }
    };

    // Create server manager
    let cfg = config.lock().unwrap();
    let server = Arc::new(ServerManager::new(
        &cfg.server_path,
        &cfg.model_path,
        cfg.port,
        cfg.n_gpu_layers,
        cfg.n_ctx,
        cfg.threads,
    ));
    drop(cfg);

    // Create chat client
    let client = Arc::new(Mutex::new(ChatClient::new(&format!(
        "http://127.0.0.1:{}",
        config.lock().unwrap().port
    ))));

    eframe::run_native(
        "WuffAgent",
        options,
        Box::new(|_cc| Ok(Box::new(ChatApp::new(server, client, config)))),
    )
}
