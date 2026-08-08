# Phase 6: Settings Panel

## Status: Pending

---

### Step 6.1: Settings Window

**Objective: Settings dialog with server config fields.

**Tasks:
- Create `internal/ui/settings.go`
- Add fields for server_path, model_path, port, gpu_layers, ctx_size, threads, system_prompt
- Add save, cancel, start, stop buttons

```go
package ui

import (
    "fyne.io/fyne/v2"
    "fyne.io/fyne/v2/container"
    "fyne.io/fyne/v2/widget"
)

type SettingsDialog struct {
    dialog     fyne.Window
    config     *config.Config
    serverMgr   *server.ServerManager
    entries    map[string]*widget.Entry
    sliders    map[string]*widget.Slider
    checkboxes map[string]*widget.Checkbox
}

func NewSettingsDialog(parent fyne.Window, cfg *config.Config, srv *server.ServerManager) *SettingsDialog {
    d := &SettingsDialog{
        config: cfg,
        serverMgr: srv,
    }
    d.setup()
    return d
}

func (d*SettingsDialog) setup() {
    d.entries = map[string]*widget.Entry{
        "server_path": widget.NewEntry(),
        "model_path": widget.NewEntry(),
        "port": widget.NewEntry(),
        "threads": widget.NewEntry(),
        "n_ctx": widget.NewEntry(),
        "system_prompt": widget.NewMultiLineEntry(),
    }
    d.entries["server_path"].SetValue(d.config.ServerPath)
    d.entries["model_path"].SetValue(d.config.ModelPath)
    d.entries["port"].SetValue(fmt.Sprintf("%d", d.config.Port))
    // ...

    d.dialog = fyne.CurrentApp().NewWindow("Settings")
    d.dialog.Resize(fyne.NewSize(500, 600))
}
```

**Success Criteria:
- Settings window opens
- All fields visible
- Save writes to JSON

**Dependencies: Step 2.1 (config struct exists)

---

### Step 6.2: File Pickers

**Objective: Allow browsing for server and model paths.

**Tasks:
- Add file picker dialogs for server_path, model_path
- Validate selected file exists
- Update config fields

```go
func (d*SettingsDialog) openFilePicker(field string) {
    dialog.ShowFileDialog(func(reader dialog.File) {
        // ...
    }, func(err error) {
        // Handle cancellation
    })
    dialog.SetFilter(&dialog.FileFilter{
        Patterns: []string{"*.exe", "*.dll"},
    })
}
```

**Success Criteria:
- File picker opens
- Selected paths are saved

**Dependencies: Step 6.1

---

### Step 6.3: Server Control

**Objective: Start/stop server from settings.

**Tasks:
- Wire start/stop buttons to server manager
- Show status in settings
- Validate paths before starting

```go
func (d*SettingsDialog) startServer() {
    if d.config.ServerPath == "" || d.config.ModelPath == "" {
        widget.ShowError("Missing server or model path", d.dialog)
        return
    }
    if err := d.serverMgr.StartServer(d.config); err != nil {
        widget.ShowError("Server start failed", err.Error(), d.dialog)
        return
    }
    widget.ShowInformation("Server started", "llama-server is now running", d.dialog)
}
```

**Success Criteria:
- Server starts with selected paths
- Server stops correctly

**Dependencies: Step 3.1 (server manager)

---

## Files Created:
- `internal/ui/settings.go`

## Dependencies on other phases:
- Phase 2 (config struct)
- Phase 3 (server manager)

## Review Notes:
- File pickers use Fyne's `filedialog.New()
- Validation checks paths exist before saving
- Server control validates paths before starting
- Settings window is modal (blocks main window)
