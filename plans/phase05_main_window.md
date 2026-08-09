# Phase 5: Main Window (Rust/egui)

## Status: Pending

---

### Step 5.1: App Skeleton

**Objective**: Create basic eframe app with layout.

**Tasks**:
- Create `src/ui/window.rs`
- Create `ChatApp` struct implementing `eframe::App`
- Set window title, size
- Add placeholder content

```rust
use eframe::egui;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::server::ServerManager;
use crate::client::ChatClient;

#[derive(Debug, Clone, PartialEq)]
enum ServerStatus {
    Stopped,
    Connecting,
    Ready,
    Generating,
    Error(String),
}

pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct ChatApp {
    server: Arc<ServerManager>,
    client: Arc<Mutex<ChatClient>>,
    config: Arc<Mutex<Config>>,
    
    // UI state
    chat_display: Vec<ChatMessage>,
    input_text: String,
    is_generating: bool,
    status: ServerStatus,
    streaming: bool,
    show_settings: bool,
    progress: f32,
    
    // For streaming
    current_response: String,
}

impl ChatApp {
    pub fn new(
        server: Arc<ServerManager>,
        client: Arc<Mutex<ChatClient>>,
        config: Arc<Mutex<Config>>,
    ) -> Self {
        let cfg = config.lock().unwrap();
        Self {
            server,
            client,
            config,
            chat_display: Vec::new(),
            input_text: String::new(),
            is_generating: false,
            status: ServerStatus::Stopped,
            streaming: cfg.streaming,
            show_settings: false,
            progress: 0.0,
            current_response: String::new(),
        }
    }
    
    fn setup_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("WuffAgent");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                    }
                });
            });
        });
        
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            self.draw_status_bar(ui);
        });
        
        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_chat_area(ui);
            self.draw_input_area(ui);
        });
    }
}

impl eframe::App for ChatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.setup_ui(ctx);
        
        if self.show_settings {
            crate::ui::settings::show_settings_dialog(
                ctx,
                &mut self.show_settings,
                &self.server,
                &self.client,
                &self.config,
            );
        }
    }
    
    fn save(&mut self, _storage: &dyn eframe::Storage) {
        // Save config here if needed
    }
}
```

**Success Criteria**:
- App opens, titled "WuffAgent"
- Content is visible

**Dependencies**: Step 2.1 (config), Step 3.1 (server manager available)

---

### Step 5.2: Chat Display Area

**Objective**: Scrollable chat message history.

**Tasks**:
- Add `egui::ScrollArea` with message list
- Implement `draw_chat_area()` to add messages
- Auto-scroll to bottom on new messages

```rust
impl ChatApp {
    fn draw_chat_area(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([0.0, 0.0])
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    for msg in &self.chat_display {
                        self.draw_message(ui, msg);
                    }
                    
                    // Show current streaming response
                    if self.is_generating && !self.current_response.is_empty() {
                        egui::widgets::RichText::new(&self.current_response)
                            .text_color(egui::color::PALE_BLUE)
                            .show(ui);
                        ui.separator();
                    }
                    
                    ui.allocate_ui_with_layout(
                        egui::Vec2::ZERO,
                        egui::Layout::top_down(egui::Align::Left),
                    );
                });
            });
    }
    
    fn draw_message(&mut self, ui: &mut egui::Ui, msg: &ChatMessage) {
        let is_user = msg.role == "user";
        
        ui.horizontal(|ui| {
            if is_user {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label("You: ");
            } else {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label("AI:  ");
            }
            
            let color = if is_user {
                egui::Color32::from_rgb(200, 200, 200)
            } else {
                egui::Color32::from_rgb(150, 200, 150)
            };
            
            ui.colored_label(egui::RichText::new(&msg.content).color(color), "");
        });
        ui.separator();
    }
    
    pub fn add_message(&mut self, role: &str, content: &str) {
        self.chat_display.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        });
    }
}
```

**Success Criteria**:
- Messages display in order
- Scrollbar appears when content overflows
- New messages are visible

**Dependencies**: Step 5.1

---

### Step 5.3: Input Area

**Objective**: User input field and send button.

**Tasks**:
- Add `egui::TextEdit` for input (multi-line)
- Add `egui::Button` for send
- Add `egui::Button` for stop
- Wire send button to submit
- Wire stop button to cancel

```rust
impl ChatApp {
    fn draw_input_area(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [ui.available_width() - 120.0, 80.0],
                    egui::TextEdit::singleline(&mut self.input_text)
                        .hint_text("Type your message...")
                        .multi_line(true),
                );
            });
            
            ui.vertical(|ui| {
                if !self.is_generating {
                    if ui.button("Send").clicked() {
                        self.send_message();
                    }
                } else {
                    if ui.button("Stop").clicked() {
                        self.stop_generation();
                    }
                }
                
                if ui.button("Clear").clicked() {
                    self.clear_chat();
                }
            });
        });
    }
    
    fn send_message(&mut self) {
        let text = self.input_text.trim().to_string();
        if text.is_empty() {
            return;
        }
        
        self.input_text.clear();
        self.add_message("user", &text);
        
        let server = self.server.clone();
        let client = self.client.clone();
        let status = egui::Mutex::new(ServerStatus::Generating);
        
        // Spawn async task (using egui's built-in async support)
        ctx.request_repaint();
        
        // Note: In a real app, you'd use eframe's async integration
        // or spawn a tokio task and poll it
    }
    
    fn stop_generation(&mut self) {
        // Cancel streaming
        tokio::task::block_in_place(|| {
            self.client.blocking_lock().stop_generation();
        });
        
        self.is_generating = false;
        self.current_response.clear();
    }
    
    fn clear_chat(&mut self) {
        self.chat_display.clear();
        tokio::task::block_in_place(|| {
            self.client.blocking_lock().clear_history();
        });
    }
}
```

**Success Criteria**:
- User can type in input field
- Send button triggers submission
- Stop button is visible during generation

**Dependencies**: Step 5.1

---

### Step 5.4: Status Bar

**Objective**: Show server state (connecting, ready, generating, error).

**Tasks**:
- Add status label
- Update on server state changes
- Color-code: green=ready, blue=generating, red=error, gray=connecting

```rust
impl ChatApp {
    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        let (text, color) = match &self.status {
            ServerStatus::Stopped => ("● Stopped", egui::Color32::GRAY),
            ServerStatus::Connecting => ("● Connecting...", egui::Color32::BLUE),
            ServerStatus::Ready => ("● Ready", egui::Color32::GREEN),
            ServerStatus::Generating => ("● Generating...", egui::Color32::BLUE),
            ServerStatus::Error(e) => (&format!("● Error: {}", e), egui::Color32::RED),
        };
        
        ui.label(egui::RichText::new(text).color(color));
    }
    
    pub fn set_status(&mut self, status: ServerStatus) {
        self.status = status;
    }
}
```

**Success Criteria**:
- Status text changes based on state
- Colors are visible

**Dependencies**: Step 5.1

---

### Step 5.5: Bottom Bar

**Objective**: Settings, Clear, and exit buttons.

**Tasks**:
- Add settings button to menu bar (already done in 5.1)
- Add clear button in input area
- Add exit button

```rust
impl ChatApp {
    fn draw_exit_button(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("exit_panel").show(ctx, |ui| {
            if ui.button("Exit").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}
```

**Success Criteria**:
- All buttons visible
- Buttons are functional

**Dependencies**: Step 5.2 (chat display exists)

---

## Files Created:
- `src/ui/window.rs`

## Dependencies on other phases:
- Phase 2 (config)
- Phase 3 (server manager)
- Phase 4 (chat client)

## Review Notes:
- eframe::App trait is the main interface
- egui::ScrollArea for chat history
- egui::TextEdit for input (multi_line)
- Status bar uses TopBottomPanel
- Color coding for status indicators
- Async operations need careful handling in egui (use `ctx.spawn` or external tokio tasks)
- `eframe::Storage` for saving app state
