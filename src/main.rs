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
    // Create a Tokio runtime so tokio::spawn works inside the app
    let _rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = _rt.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };

    // Load config
    let config_path = config::get_config_path();
    let config = match Config::load(&config_path) {
        Ok(mut cfg) => {
            cfg.file_path = config_path.clone();
            Arc::new(Mutex::new(cfg))
        }
        Err(e) => {
            eprintln!("Failed to load config: {}, using defaults", e);
            let mut default_cfg = Config::default();
            default_cfg.file_path = config_path.clone();
            Arc::new(Mutex::new(default_cfg))
        }
    };

    // Create server manager: real instance for local mode, noop for remote
    let is_remote;
    {
        let cfg = config.lock().unwrap();
        is_remote = cfg.is_remote();
    }

    let server = if is_remote {
        Arc::new(ServerManager::noop())
    } else {
        let cfg = config.lock().unwrap();
        Arc::new(ServerManager::new(
            &cfg.server_path,
            &cfg.model_path,
            cfg.port,
            cfg.n_gpu_layers,
            cfg.n_ctx,
            cfg.threads,
        ))
    };

    // Create chat client using config's base_url()
    let base_url;
    let api_key;
    {
        let cfg = config.lock().unwrap();
        base_url = cfg.base_url();
        api_key = cfg.remote_api_key.clone();
    }
    let client = Arc::new(Mutex::new(ChatClient::new(&base_url)));

    // Set API key for remote connections
    {
        let mut cl = client.lock().unwrap();
        cl.set_api_key(api_key.as_deref());
    }

    eframe::run_native(
        "WuffAgent",
        options,
        Box::new(|_cc| Ok(Box::new(ChatApp::new(server, client, config)))),
    )
}
