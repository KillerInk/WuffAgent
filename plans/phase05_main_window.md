# Phase 5: Main Window

## Status: Pending

---

### Step 5.1: Window Skeleton

**Objective: Create basic Fyne window with layout.

**Tasks:
- Create `internal/ui/window.go`
- Create `NewChatWindow()` returning `*ChatWindow`
- Set window title, size
- Add placeholder content

```go
package ui

import (
    "fyne.io/fyne/v2/app"
    "fyne.io/fyne/v2/container"
    "fyne.io/fyne/v2/widget"
)

type ChatWindow struct {
    fyneWindow fyne.Window
    app      fyne.App
    config    *config.Config
    server    *server.ServerManager
    client    *client.ChatClient
    chatDisplay *widget.Scroll
    inputField  *widget.Entry
    sendBtn    *widget.Button
    stopBtn    *widget.Button
    statusLabel *widget.Label
}

func NewChatWindow(app fyne.App, cfg *config.Config, srv *server.ServerManager, cli *client.ChatClient) *ChatWindow {
    w := &ChatWindow{
        app:  app,
        fyneWindow: app.NewWindow("WuffAgent"),
        config: cfg,
        server: srv,
        client: cli,
    }
    w.setupLayout()
    return w
}

func (w*ChatWindow) setupLayout() {
    // Main layout
    w.fyneWindow.SetContent(container.NewBorder(
        w.buildHeader(),
        nil,
        w.buildFooter(),
        nil,
        w.buildChatArea(),
    ))
    w.fyneWindow.Resize(fyne.NewSize(900, 700))
    w.fyneWindow.CenterOnScreen()
}
```

**Success Criteria:
- Window opens, titled "WuffAgent"
- Content is visible

**Dependencies: Step 1.2 (directory structure exists)

---

### Step 5.2: Chat Display Area

**Objective: Scrollable chat message history.

**Tasks:
- Add `widget.Scroll` with `widget.RichText` for display
- Implement `DisplayMessage()` to add messages
- Add scrollbar, auto-scroll

```go
func (w*ChatWindow) buildChatArea() fyne.CanvasObject {
    w.chatDisplay = widget.NewScroll(container.NewVBox())
    w.chatDisplay.SetMinSize(fyne.NewSize(800, 500))
    return w.chatDisplay
}

func (w*ChatWindow) DisplayMessage(role string, text string) {
    // Add message bubble
    // Auto-scroll to bottom
}
```

**Success Criteria:
- Messages display in order
- Scrollbar appears when content overflows
- New messages are visible

**Dependencies: Step 5.1

---

### Step 5.3: Input Area

**Objective: User input field and send button with validation.

**Tasks:
- Add `widget.Entry` for input (multi-line)
- Add `widget.Button` for send
- Add `widget.Button` for stop
- Wire send button to submit
- Wire stop button to cancel
- Add input validation (empty check, max length)

```go
func (w*ChatWindow) buildFooter() fyne.CanvasObject {
    w.inputField = widget.NewMultiLineEntry()
    w.inputField.PlaceHolder = "Type your message...

    w.sendBtn = widget.NewButton("Send", func() {
        text := w.inputField.Text
        if strings.TrimSpace(text) == "" {
            return
        }
        w.sendMessage(text)
    })
    w.stopBtn = widget.NewButton("Stop", func() {
        w.stopGeneration()
    })
    w.stopBtn.Hide() // Hidden initially, shown during generation

    return container.NewHBox(
        w.inputField,
        w.stopBtn,
        w.sendBtn,
    )
}

func (w*ChatWindow) sendMessage(text string) {
    if strings.TrimSpace(text) == "" {
        return
    }
    // Disable input during generation
    w.inputField.Disable()
    w.sendBtn.Disable()
    w.stopBtn.Show()
    w.stopBtn.Enable()

    // Send via client
    go func() {
        // CallLater for UI safety
        // ...
    }()
}

func (w*ChatWindow) stopGeneration() {
    w.client.StopGeneration()
    w.inputField.Enable()
    w.sendBtn.Enable()
    w.stopBtn.Hide()
}
```

**Success Criteria:
- User can type in input field
- Send button triggers submission
- Stop button is visible during generation
- Input is validated (empty check)

**Dependencies: Step 5.1

---

### Step 5.4: Status Bar

**Objective: Show server state (connecting, ready, generating, error).

**Tasks:
- Add `widget.Label` for status
- Update on server state changes
- Color-code: green=ready, blue=generating, red=error, gray=connecting

```go
func (w*ChatWindow) buildHeader() fyne.CanvasObject {
    w.statusLabel = widget.NewLabel("● Connecting")
    // Color coding
    return container.NewHBox(
        widget.NewLabel("WuffAgent"),
        fyne.NewContainer(),
        w.statusLabel,
    )
}

func (w*ChatWindow) setStatus(status Status, text string) {
    w.statusLabel.Text = text
    w.statusLabel.Refresh()
    switch status {
    case StatusReady:
        w.statusLabel.Style.TextColor = color.Green
    case StatusGenerating:
        w.statusLabel.Style.TextColor = color.Blue
    case StatusError:
        w.statusLabel.Style.TextColor = color.Red
    case StatusConnecting:
        w.statusLabel.Style.TextColor = color.Gray
    }
}
```

**Success Criteria:
- Status text changes based on state
- Colors are visible

**Dependencies: Step 5.1

---

### Step 5.5: Bottom Bar

**Objective: Settings, Clear, and theme buttons.

**Tasks:
- Add `widget.Button` for settings
- Add `widget.Button` for clear history
- Add `widget.Button` for theme toggle

```go
func (w*ChatWindow) buildFooter() fyne.CanvasObject {
    return container.NewHBox(
        widget.NewButton("⚙ Settings", func() {
            w.openSettings()
        }),
        widget.NewButton("🗑 Clear", func() {
            w.clearHistory()
        }),
        w.stopBtn,
        w.sendBtn,
        widget.NewButton("🌓 Theme", func() {
            w.toggleTheme()
        }),
    )
}

func (w*ChatWindow) clearHistory() {
    w.client.ClearHistory()
    // Clear display
}
```

**Success Criteria:
- All buttons visible
- Buttons are functional

**Dependencies: Step 5.2 (chat display exists)

---

### Step 5.6: Model Loading Progress

**Objective: Show loading progress during model loading.

**Tasks:
- Add progress bar for model loading
- Show percentage
- Disable input during loading

```go
type ChatWindow struct {
    loadingProgress *widget.ProgressBar
    // ...
}

func (w*ChatWindow) showLoading() {
    w.loadingProgress.Show()
    w.inputField.Disable()
}

func (w*ChatWindow) hideLoading() {
    w.loadingProgress.Hide()
    w.inputField.Enable()
}
```

**Success Criteria:
- Progress bar shows during model loading
- Input is disabled during loading

**Dependencies: Step 5.1

---

### Step 5.7: Keyboard Shortcuts

**Objective: Ctrl+Enter to send, Escape to stop.

**Tasks:
- Add `widget.Entry` shortcuts
- Ctrl+Enter sends message
- Escape stops generation

```go
func (w*ChatWindow) setupShortcuts() {
    w.inputField.OnSubmitted = func(s string) {
        // Enter sends
        w.sendMessage(s)
    }
}
```

**Success Criteria:
- Ctrl+Enter sends
- Escape stops generation

**Dependencies: Step 5.3

---

## Files Created:
- `internal/ui/window.go`
- `internal/ui/window.go` (main chat window)

## Dependencies on other phases:
- Phase 2 (config)
- Phase 3 (server manager)
- Phase 4 (chat client)

## Review Notes:
- All UI updates from goroutines MUST use `app.CurrentApp().CallLater()` for thread safety
- Input validation: empty messages rejected
- Clear button clears client-side display
- Model info from server logs displayed in status bar
- Theme toggle uses Fyne theme switching