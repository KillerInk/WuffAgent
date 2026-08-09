# Phase 7: Integration (Rust)

## Status: Pending

---

### Step 7.1: Wire Config to UI

**Objective**: Load config at startup, save on close.

**Tasks**:
- `main.rs` loads config
- Pass config to app
- Save config on app close

```rust
use std::sync::{Arc, Mutex};
use tracing_subscriber::{EnvFilter, prelude::*};

mod config;
mod server;
mod client;
mod ui;

use config::{Config, get_config_path};
use server::ServerManager;
use client::ChatClient;
use ui::window::ChatApp;

#[tokio::main]
async fn main() -> eframe::Result {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load config
    let config_path = get_config_path();
    let config = match Config::load(&config_path) {
        Ok(cfg) => Arc::new(Mutex::new(cfg)),
        Err(e) => {
            tracing::warn!("Failed to load config: {}, using defaults", e);
            let cfg = Config::default();
            Arc::new(Mutex::new(cfg))
        }
    };

    let cfg = config.lock().unwrap();
    
    // Create server manager
    let server = Arc::new(ServerManager::new(
        &cfg.server_path,
        &cfg.model_path,
        cfg.port,
        cfg.n_gpu_layers,
        cfg.n_ctx,
        cfg.threads,
    ));
    
    // Create chat client
    let client = Arc::new(Mutex::new(ChatClient::new(
        &format!("http://127.0.0.1:{}", cfg.port)
    )));
    
    drop(cfg);
    
    // Create app
    let app = ChatApp::new(server, client, config);
    
    // Run eframe
    let options = eframe::AppOptions {
        viewport: egui::ViewportBuilder::default()
            .with_size(900, 700)
            .with_title("WuffAgent"),
        ..Default::default()
    };
    
    eframe::run_native("WuffAgent", options, Box::new(|cc| Ok(Box::new(app))))
}
```

**Success Criteria**:
- Settings persist between runs
- App uses loaded config

**Dependencies**: Step 5.1, Step 6.1

---

### Step 7.2: Wire Client to Server

**Objective**: Chat client connects to running server.

**Tasks**:
- App sends messages via client
- Client uses config for base URL

```rust
impl ChatApp {
    async fn send_message_async(&mut self, text: &str) {
        self.status = ServerStatus::Generating;
        self.is_generating = true;
        
        if self.streaming {
            self.send_streaming(text).await;
        } else {
            self.send_non_streaming(text).await;
        }
        
        self.is_generating = false;
    }
    
    async fn send_non_streaming(&mut self, text: &str) {
        let client = self.client.clone();
        let result = tokio::task::spawn(async move {
            client.lock().await.send_message(text).await
        }).await;
        
        match result {
            Ok(Ok(content)) => {
                self.add_message("assistant", &content);
                self.status = ServerStatus::Ready;
            }
            Ok(Err(e)) => {
                self.status = ServerStatus::Error(e.to_string());
                tracing::error!("Send message error: {}", e);
            }
            Err(e) => {
                self.status = ServerStatus::Error(e.to_string());
                tracing::error!("Task error: {}", e);
            }
        }
    }
    
    async fn send_streaming(&mut self, text: &str) {
        let client = self.client.clone();
        let mut response = String::new();
        
        let result = tokio::task::spawn(async move {
            client.lock().await.stream_message(text, |chunk| {
                response.push_str(&chunk);
                Ok(())
            }).await
        }).await;
        
        match result {
            Ok(Ok(())) => {
                self.add_message("assistant", &response);
                self.status = ServerStatus::Ready;
            }
            Ok(Err(e)) => {
                self.status = ServerStatus::Error(e.to_string());
                tracing::error!("Stream error: {}", e);
            }
            Err(e) => {
                self.status = ServerStatus::Error(e.to_string());
                tracing::error!("Task error: {}", e);
            }
        }
        
        self.current_response.clear();
    }
}
```

**Success Criteria**:
- Messages go to server
- Responses come back

**Dependencies**: Step 4.1, Step 5.1

---

### Step 7.3: End-to-End Flow

**Objective**: Complete chat cycle works.

**Tasks**:
- User types message
- Client sends to server
- Response displays in chat
- History updates

```rust
impl ChatApp {
    fn send_message(&mut self) {
        let text = self.input_text.trim().to_string();
        if text.is_empty() {
            return;
        }
        
        self.add_message("user", &text);
        self.input_text.clear();
        
        // Note: In a real eframe app, you'd use ctx.spawn for async
        // For now, this is a placeholder
        tracing::info!("Sending message: {}", text);
    }
}
```

**Success Criteria**:
- Full round-trip works
- Chat history updates

**Dependencies**: All previous phases

---

## Files Modified:
- `src/main.rs`
- `src/ui/window.rs`

## Dependencies on other phases:
- Phase 4 (chat client)
- Phase 5 (UI window)

## Review Notes:
- Config loaded from file at startup
- Server started after config loads
- On close: save config, stop server
- Error handling for config load failures
- Graceful shutdown on app close
- Async operations need careful integration with eframe
