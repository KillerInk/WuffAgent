# Phase 9: Polish (Rust)

## Status: Pending

---

### Step 9.1: Theme Support

**Objective**: Light/dark theme toggle.

**Tasks**:
- Add theme button to UI
- Toggle egui theme
- Persist preference in config

```rust
impl ChatApp {
    fn toggle_theme(&mut self, ctx: &egui::Context) {
        let cfg = self.config.lock().unwrap();
        let current = cfg.theme.clone();
        drop(cfg);
        
        let new_theme = if self.config.lock().unwrap().theme == "dark" {
            "light".to_string()
        } else {
            "dark".to_string()
        };
        
        self.config.lock().unwrap().theme = new_theme.clone();
        self.config.lock().unwrap().save().unwrap();
        
        let visuals = if new_theme == "dark" {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        
        ctx.set_visuals(visuals);
    }
}
```

**Success Criteria**:
- Theme changes are visible
- Theme persists between sessions

**Dependencies**: Step 5.1

---

### Step 9.2: Error Handling

**Objective**: Robust error recovery.

**Tasks**:
- Show error dialogs for failures
- Handle server disconnects
- Auto-reconnect if server drops

```rust
impl ChatApp {
    fn handle_error(&mut self, err: &str) {
        self.status = ServerStatus::Error(err.to_string());
        
        // Show error dialog
        // In eframe, use egui::Window or egui::Area for alerts
    }
    
    async fn auto_reconnect(&mut self) {
        // Try to reconnect after 5 seconds
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        
        // Try to start server
        // Update status
    }
}
```

**Success Criteria**:
- Errors are caught
- User is informed
- Recovery is possible

**Dependencies**: All phases

---

### Step 9.3: Context Persistence

**Objective**: Save chat history between sessions.

**Tasks**:
- Add chat history to config
- Save on close, load on startup

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    // ... existing fields ...
    pub chat_history: Vec<ChatMessage>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}
```

**Success Criteria**:
- Chat history survives restarts

**Dependencies**: Step 2.1 (config), Step 5.1 (UI)

---

### Step 9.4: Input Validation

**Objective**: Validate user input.

**Tasks**:
- Empty message check
- Max message length check

```rust
impl ChatApp {
    const MAX_MESSAGE_LENGTH: usize = 4000;
    
    fn validate_input(&self, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("Message cannot be empty".to_string());
        }
        if text.len() > Self::MAX_MESSAGE_LENGTH {
            return Err(format!("Message too long (max {} characters)", Self::MAX_MESSAGE_LENGTH));
        }
        Ok(())
    }
}
```

**Success Criteria**:
- Invalid input is rejected

**Dependencies**: Step 5.3

---

### Step 9.5: Model Info Display

**Objective**: Show model name, context size, GPU layers.

**Tasks**:
- Parse server startup logs for model info
- Display in status bar

```rust
impl ChatApp {
    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (text, color) = match &self.status {
                ServerStatus::Stopped => ("● Stopped", egui::Color32::GRAY),
                ServerStatus::Connecting => ("● Connecting...", egui::Color32::BLUE),
                ServerStatus::Ready => ("● Ready", egui::Color32::GREEN),
                ServerStatus::Generating => ("● Generating...", egui::Color32::BLUE),
                ServerStatus::Error(e) => (&format!("● Error: {}", e), egui::Color32::RED),
            };
            ui.label(egui::RichText::new(text).color(color));
            
            // Show model info
            ui.separator();
            ui.label(format!("Ctx: {} | GPU: {} | Threads: {}", 
                self.config.lock().unwrap().n_ctx,
                self.config.lock().unwrap().n_gpu_layers,
                self.config.lock().unwrap().threads,
            ));
        });
    }
}
```

**Success Criteria**:
- Model info visible

**Dependencies**: Step 3.1, Step 5.4

---

## Files Modified:
- `src/ui/window.rs`
- `src/ui/settings.rs`
- `src/config/mod.rs`

## Dependencies on other phases:
- All phases

## Review Notes:
- Theme persisted in config
- Error dialogs use egui windows
- Auto-reconnect attempts after server disconnect
- Chat history saved/loaded with config
- Input validation: empty check, max length check
- Model info from config
