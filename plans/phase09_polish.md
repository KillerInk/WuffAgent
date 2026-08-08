# Phase 9: Polish

## Status: Pending

---

### Step 9.1: Theme Support

**Objective: Light/dark theme toggle.

**Tasks:
- Add theme button to UI
- Toggle Fyne theme
- Persist preference in config

```go
func (w*ChatWindow) toggleTheme() {
    if w.config.Theme == "dark" {
        w.config.Theme = "light"
        a.Settings().SetTheme(&theme.LightTheme{})
    } else {
        w.config.Theme = "dark"
        a.Settings().SetTheme(&theme.DarkTheme{})
    }
    w.config.Save()
}
```

**Success Criteria Criteria:
- Theme changes are visible
- Theme persists between sessions

**Dependencies: Step 5.1

---

### Step 9.2: Error Handling

**Objective: Robust error recovery.

**Tasks:
- Show error dialogs for failures
- Handle server disconnects
- Auto-reconnect if server drops

```go
func (w*ChatWindow) handleError(err error) {
    widget.ShowError("Error", err.Error(), w.fyneWindow)
    w.setStatus(StatusError, "Error")
    // Try to reconnect
    go func() {
        // Auto-reconnect after 5 seconds
        time.Sleep(5 * time.Second)
        fyne.CurrentApp().CallLater(func() {
            if err := w.server.StartServer(); err == nil {
                w.setStatus(StatusReady, "● Connected")
            }
        })
    }()
}
```

**Success Criteria:
- Errors are caught
- User is informed
- Recovery is possible

**Dependencies: All phases

---

### Step 9.3: Context Persistence

**Objective: Save chat history between sessions.

**Tasks:
- Export/import chat history
- Save on close, load on startup
- Add to config struct

```go
type Config struct {
    // ...
    ChatHistory []ChatMessage `json:"chat_history"`
}

type ChatMessage struct {
    Role    string `json:"role"`
    Content string `json:"content"`
}
```

**Success Criteria:
- Chat history survives restarts

**Dependencies: Step 2.1 (config), Step 5.1 (UI)

---

### Step 9.4: Input Validation

**Objective: Validate user input.

**Tasks:
- Empty message check
- Max message length check
- Special characters handling

**Success Criteria:
- Invalid input is rejected

**Dependencies: Step 5.3

---

### Step 9.5: Model Info Display

**Objective: Show model name, context size, GPU layers.

**Tasks:
- Parse server startup logs for model info
- Display in status bar

**Success Criteria:
- Model info visible

**Dependencies: Step 3.1, Step 5.4

---

## Files Modified:
- `internal/ui/window.go`
- `internal/ui/settings.go`
- `internal/config/config.go`

## Dependencies on other phases:
- All phases

## Review Notes:
- Theme persisted in config
- Error dialogs use Fyne's widget.ShowError()
- Auto-reconnect attempts after server disconnect
- Chat history saved/loaded with config
- Input: empty check, max length check
- Model info from server logs
