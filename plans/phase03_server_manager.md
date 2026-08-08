# Phase 3: Server Manager

## Status: Pending

---

### Step 3.1: Process Lifecycle

**Objective: Start and stop llama-server process.

**Tasks:
- Create `internal/server/manager.go`
- Implement `ServerManager` struct
- `StartServer()` spawns `llama-server` with config params
- `StopServer()` sends SIGTERM, waits for exit
- `IsRunning()` checks process state
- `WaitForReady()` polls HTTP endpoint

```go
package server

import (
    "context"
    "encoding/json"
    "fmt"
    "net/http"
    "os/exec"
    "sync"
    "sync/atomic"
    "time"
)

type ServerManager struct {
    ServerPath string
    ModelPath  string
    Port       int
    GPU        int
    N_CTX      int
    Threads    int
    process    *exec.Cmd
    mu         sync.Mutex
    running    atomic.Bool
    ctx       context.Context
    cancel     context.CancelFunc
    started    bool
}

func NewServerManager(cfg Config) *ServerManager {
    ctx, cancel := context.WithCancel(context.Background())
    return &ServerManager{
        ServerPath: cfg.ServerPath,
        ModelPath:  cfg.ModelPath,
        Port:       cfg.Port,
        GPU:      cfg.GPULayers,
        N_CTX:    cfg.N_CTX,
        Threads: cfg.Threads,
        ctx:     ctx,
        cancel:  cancel,
    }
}

func (s *ServerManager) StartServer() error {
    s.mu.Lock()
    defer s.mu.Unlock()

    // Build arguments
    args := []string{
        "--model", s.ModelPath,
        "--port", fmt.Sprintf("%d", s.Port),
        "--host", "127.0.0.1",
        "--threads", fmt.Sprintf("%d", s.Threads),
        "--n-gpu-layers", fmt.Sprintf("%d", s.GPU),
        "--n_ctx", fmt.Sprintf("%d", s.N_CTX),
    }

    s.process = exec.CommandContext(ctx, s.ServerPath, args...)
    s.process.Stdout = os.Stdout
    s.process.Stderr = os.Stderr

    if err := s.process.Start(); err != nil {
        return fmt.Errorf("failed to start server: %w", err)
    }

    s.running.Store(true)

    // Monitor process exit
    go func() {
        s.process.Wait()
        s.running.Store(false)
    }()

    return nil
}

func (s*ServerManager) StopServer() error {
    if !s.running.Load() {
        return nil
    }
    s.cancel()
    s.process.Signal(os.SIGTERM)
    s.process.Wait()
    return nil
}

func (s*ServerManager) IsRunning() bool {
    return s.running.Load()
}

func (s*ServerManager) WaitForReady(timeout time.Duration) error {
    deadline := time.Now().Add(timeout)
    for time.Now().Before(deadline) {
        _, err := http.Get(fmt.Sprintf("http://127.0.0.1:%d/health", s.Port))
        if err == nil {
            return nil
        }
        time.Sleep(500 * time.Millisecond)
    }
    return fmt.Errorf("server not ready after %v", timeout)
}
```

**Success Criteria:
- Server process starts with correct arguments
- Process is running after `StartServer()`
- Server stops after `StopServer()`
- `IsRunning()` returns correct boolean
- `WaitForReady()` returns true when server responds

**Dependencies: Step 2.1 (config available)

---

### Step 3.2: Process Monitoring

**Objective: Detect server crashes, handle errors.

**Tasks:
- Monitor `Stderr` for error output
- Implement `Wait()` to detect process exit
- Log server output to console

```go
func (s*ServerManager) GetStatus() ServerStatus {
    type ServerStatus struct {
        Running    bool
        PID      int
        Started  time.Time
        Error   error
    }
    // ...
}
```

**Success Criteria:
- Crash detection works
- Error messages are visible
- Process state is tracked

**Dependencies: Step 3.1

---

### Step 3.3: Model Loading Progress

**Objective: Show loading progress during model loading.

**Tasks:
- Parse stderr for loading progress messages from llama-server
- Expose progress channel for UI updates

**Success Criteria:
- Progress percentage available

**Dependencies: Step 3.1

---

## Files Created:
- `internal/server/manager.go`

## Dependencies on other phases:
- Phase 2 (config provides paths, port, GPU, threads, n_ctx)
- Phase 5 (UI needs progress indicator)

## Review Notes:
- `os/exec.Cmd` is used for process management
- `atomic.Bool` for thread-safe running state
- `context.Context` for cancellation
- stderr monitoring for crash detection
- Progress parsing from stderr (llama-server outputs loading info)
- `WaitForReady` uses HTTP polling of `/health` endpoint
- SIGTERM for graceful shutdown
- Process.Wait() runs in goroutine to detect exits without blocking main thread
- Windows: `os.SIGTERM` works, but `process.Kill()` may be needed for forceful shutdown on Windows
