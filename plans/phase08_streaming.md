# Phase 8: Streaming Integration (Rust)

## Status: Pending

---

### Step 8.1: Streaming to UI

**Objective**: Streaming responses update UI in real time.

**Tasks**:
- Wire `stream_message` to chat display
- Each token chunk appears immediately
- Progress updates as tokens arrive
- Use `egui::Context::request_repaint()` for responsive UI

```rust
impl ChatApp {
    async fn send_streaming_with_ui(&mut self, text: &str) {
        self.status = ServerStatus::Generating;
        self.is_generating = true;
        self.current_response.clear();
        
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
        self.is_generating = false;
    }
    
    fn request_repaint(&self, ctx: &egui::Context) {
        ctx.request_repaint();
    }
}
```

**Success Criteria**:
- Tokens show in chat area as they stream
- UI remains responsive during streaming

**Dependencies**: Step 4.3 (SSE streaming), Step 5.2 (chat display)

---

### Step 8.2: Streaming Toggle

**Objective**: User selects streaming mode.

**Tasks**:
- Settings checkbox controls streaming
- Client checks config before sending

```rust
impl ChatApp {
    fn send_message(&mut self, ctx: &egui::Context, text: &str) {
        self.add_message("user", text);
        self.input_text.clear();
        
        if self.streaming {
            // Spawn async task for streaming
            ctx.spawn(async move {
                // This would need proper app reference
            });
        } else {
            // Non-streaming
        }
    }
}
```

**Success Criteria**:
- Checkbox enables/disables streaming

**Dependencies**: Step 8.1, Step 6.1 (settings)

---

### Step 8.3: Stop Generation Implementation

**Objective**: Cancel ongoing streaming.

**Tasks**:
- Stop button aborts HTTP request
- Removes last assistant message from history
- Cleans up UI state

```rust
impl ChatApp {
    fn stop_generation(&mut self) {
        // Cancel streaming
        tokio::task::block_in_place(|| {
            self.client.blocking_lock().stop_generation();
        });
        
        // Remove incomplete assistant message
        if let Some(last) = self.chat_display.last() {
            if last.role == "assistant" && last.content.is_empty() {
                self.chat_display.pop();
            }
        }
        
        self.is_generating = false;
        self.current_response.clear();
        self.status = ServerStatus::Ready;
    }
}
```

**Success Criteria**:
- Generation stops when user clicks stop
- History is cleaned up
- UI returns to ready state

**Dependencies**: Step 8.1

---

## Files Modified:
- `src/ui/window.rs`
- `src/client/mod.rs`

## Dependencies on other phases:
- Phase 4 (chat client with streaming, StopGeneration)
- Phase 5 (UI window)

## Review Notes:
- Async streaming needs careful integration with egui
- `ctx.request_repaint()` for UI updates during streaming
- Stop generation closes HTTP response and cancels task
- Streaming mode is user-selectable in settings
- Thread safety: use `Arc<Mutex<>>` for shared state
- tokio::spawn for async operations
