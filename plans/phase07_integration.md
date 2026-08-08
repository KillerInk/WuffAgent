# Phase 7: Integration

## Status: Pending

---

### Step 7.1: Wire Config to UI

**Objective: Load config at startup, save on close.

**Tasks:
- `main.go` loads config
- Pass config to window
- Save config before exit

```go
func main() {
    a := app.New()
    
    // Load config
    cfg, err := config.LoadConfig(config.GetConfigPath())
    if err != nil {
        cfg = config.DefaultConfig()
    }
    
    // Create server manager
    srv := server.NewServerManager(cfg)
    
    // Create chat client
    cli := client.NewChatClient(fmt.Sprintf("http://127.0.0.1:%d", cfg.Port))
    
    // Create UI
    ui := ui.NewChatWindow(a, cfg, srv, cli)
    ui.Show()
    
    // Save on close
    a.OnClose = func() {
        cfg.Save()
        srv.StopServer()
    }
}
```

**Success Criteria:
- Settings persist between runs
- Window uses loaded config

**Dependencies: Step 5.1, Step 6.1

---

### Step 7.2: Wire Client to Server

**Objective: Chat client connects to running server.

**Tasks:
- Window sends messages via client
- Client uses config for base URL

**Success Criteria:
- Messages go to server
- Responses come back

**Dependencies: Step 4.1, Step 5.1

---

### Step 7.3: End-to-End Flow

**Objective: Complete chat cycle works.

**Tasks:
- User types message
- Client sends to server
- Response displays in chat
- History updates

**Success Criteria:
- Full round-trip works
- Chat history updates

**Dependencies: All previous phases

---

## Files Modified:
- `main.go`
- `internal/ui/window.go`

## Dependencies on other phases:
- Phase 4 (chat client)
- Phase 5 (UI window)

## Review Notes:
- Config loaded from file at startup
- Server started after config loads
- On close: save config, stop server
- Error handling for config load failures
- Graceful shutdown on app close
