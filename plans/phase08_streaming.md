# Phase 8: Streaming Integration

## Status: Pending

---

### Step 8.1: Streaming to UI

**Objective: Streaming responses update UI in real time.

**Tasks:
- Wire `StreamMessage` to chat display
- Each token chunk appears immediately
- Progress updates as tokens arrive
- Use `app.CurrentApp().CallLater()` for thread safety

```go
func (w*ChatWindow) startStreaming() {
    prompt := w.inputField.Text
    w.inputField.Text = ""
    
    // Add user message to display
    w.displayMessage("user", prompt)
    
    // Start assistant message placeholder
    var assistantContent string
    w.displayMessage("assistant", "") // Placeholder
    
    go func() {
        ctx, cancel := context.WithCancel(context.Background())
        defer cancel()
        
        err := w.client.StreamMessage(ctx, prompt, func(token string) error {
            assistantContent += token
            
            // Thread-safe UI update
            fyne.CurrentApp().CallLater(func() {
                w.updateAssistantMessage(token)
            })
            
            return nil
        })
        
        // Clean up after streaming
        fyne.CurrentApp().CallLater(func() {
            w.inputField.Enable()
            w.sendBtn.Enable()
            w.stopBtn.Hide()
        })
    }()
}
```

**Success Criteria:
- Tokens show in chat area as they stream
- UI remains responsive during streaming

**Dependencies: Step 4.3 (SSE streaming), Step 5.2 (chat display)

---

### Step 8.2: Streaming Toggle

**Objective: User selects streaming mode.

**Tasks:
- Settings checkbox controls streaming
- Client checks config before sending

```go
func (w*ChatWindow) sendMessage(text string) {
    if w.config.Streaming {
        w.startStreaming()
    } else {
        w.sendNonStreaming(text)
    }
}
```

**Success Criteria:
- Checkbox enables/disables streaming

**Dependencies: Step 8.1, Step 6.1 (settings)

---

### Step 8.3: Stop Generation Implementation

**Objective: Cancel ongoing streaming.

**Tasks:
- Stop button closes HTTP response
- Removes last assistant message from history
- Cleans up UI state

```go
func (w*ChatWindow) stopGeneration() {
    // Cancel streaming
    w.client.StopGeneration()
    
    // Remove incomplete assistant message
    fyne.CurrentApp().CallLater(func() {
        // Hide stop button
        w.stopBtn.Hide()
        // Enable input
        w.inputField.Enable()
        w.sendBtn.Enable()
        // Remove last assistant message from display
    })
}
```

**Success Criteria:
- Generation stops when user clicks stop
- History is cleaned up
- UI returns to ready state

**Dependencies: Step 8.1

---

## Files Modified:
- `internal/ui/window.go`
- `internal/client/chat.go`

## Dependencies on other phases:
- Phase 4 (chat client with streaming, StopGeneration)
- Phase 5 (UI window)

## Review Notes:
- All UI updates from streaming callbacks MUST use `app.CurrentApp().CallLater()` for thread safety
- Stop generation closes HTTP response body and cancels context
- Streaming mode is user-selectable in settings
- Thread safety is critical: goroutines cannot update Fyne widgets directly
- Context cancellation is used for stop generation
- History cleanup removes incomplete messages on stop
- Streaming checkbox in settings controls whether streaming or non-streaming mode is used
