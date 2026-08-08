# Phase 10: Testing

## Status: Pending

---

### Step 10.1: Unit Tests

**Objective: Core logic tests.

**Tasks:
- Test config load/save
- Test request building
- Test message parsing
- Test history truncation

```go
func TestConfigLoad(t*testing.T) {
    cfg := DefaultConfig()
    cfg.ServerPath = "C:\\llama\\llama-server.exe"
    cfg.ModelPath = "C:\\models\\model.gguf"
    
    if err := cfg.Validate(); err != nil {
        t.Errorf("Validation failed: %v", err)
    }
    
    // Save and reload
    tmpDir := t.TempDir()
    cfg.FilePath = filepath.Join(tmpDir, "config.json")
    cfg.Save()
    
    loaded, err := LoadConfig(cfg.FilePath)
    if err != nil {
        t.Fatalf("Load failed: %v", err)
    }
    
    if loaded.ServerPath != cfg.ServerPath {
        t.Errorf("Server path mismatch: got %q, want %q", loaded.ServerPath, cfg.ServerPath)
    }
}
```

**Success Criteria:
- `go test ./...` passes
- All tests pass

**Dependencies: Steps 2.1, 4.1

---

### Step 10.2: Integration Tests

**Objective: Server manager and client tests.

**Tasks:
- Test server start/stop cycle
- Test chat flow with mock server

```go
func TestServerStartStop(t*testing.T) {
    srv := NewServerManager(&Config{
        ServerPath: "llama-server.exe",
        ModelPath:  "test_model.gguf",
        Port:      18080,
        GPULayers: 0,
        N_CTX:    2048,
        Threads:   4,
    })
    
    // Start server
    if err := srv.StartServer(); err != nil {
        t.Skip("llama-server not available, skipping")
    }
    defer srv.StopServer()
    
    // Wait for ready
    if err := srv.WaitForReady(30 * time.Second); err != nil {
        t.Fatalf("Server not ready: %v", err)
    }
    
    if !srv.IsRunning() {
        t.Error("Server should be running")
    }
}
```

**Success Criteria:
- Process management works
- API calls are correct

**Dependencies: Step 3.1, Step 4.1

---

### Step 10.3: End-to-End Build

**Objective: Compile final binary.

**Tasks:
- Build with `go build`
- Test on Windows
- Verify single executable works

```powershell
# Build for Windows
go build -o WuffAgent.exe

# Verify binary exists
if (Test-Path WuffAgent.exe) {
    Write-Host "Build successful"
} else {
    Write-Host "Build failed"
    exit 1
}
```

**Success Criteria:
- Binary runs on Windows
- No external runtime needed
- Single executable works

**Dependencies: All phases

---

## Files Created:
- `internal/config/config_test.go`
- `internal/server/manager_test.go`
- `internal/client/chat_test.go`
- `internal/ui/window_test.go`

## Dependencies on other phases:
- All phases

## Review Notes:
- Unit tests use `t.TempDir()` for config
- Integration tests skip if llama-server not available
- Build produces single Windows executable
- Tests verify config load/save, request building, SSE parsing, history truncation
- Fyne UI tests are skipped if no display available
