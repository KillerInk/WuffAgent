# Phase 1: Project Foundation

## Status: Pending

---

### Step 1.1: Initialize Go Module

**Objective: Set up Go module with Fyne dependency.

**Tasks:
- Run `go mod init wuffagent`
- Create `go.mod` with `github.com/fyne-io/fyne/v2` dependency
- Create basic `main.go` with `app.New()` and `window.Create()`

```go
// main.go skeleton
package main

import (
    "fyne.io/fyne/v2/app"
)

func main() {
    a := app.New()
    w := a.NewWindow("WuffAgent")
    w.Resize(fyne.NewSize(800, 600))
    w.ShowAndRun()
}
```

**Success Criteria:
- Module initializes, `go mod tidy` succeeds
- `go run main.go` opens an empty window

**Dependencies: None

---

### Step 1.2: Create Directory Structure

**Objective: Set up internal package layout with proper Go structure.

**Tasks:
- Create `internal/server/`, `internal/client/`, `internal/ui/`, `internal/config/`
- Create empty placeholder files in each package
- Verify `go build` compiles

**Directory structure:

```
WuffAgent/
├── go.mod
├── go.sum
├── main.go
├── internal/
│   ├── config/
│   ├── client/
│   ├── server/
│   └── ui/
```

**Success Criteria:
- All directories exist
- `go build` compiles successfully

**Dependencies: Step 1.1

---

### Step 1.3: Verify Build

**Objective: Ensure project builds after directory creation.

**Tasks:
- Create placeholder files:
  - `internal/config/config.go` - empty package declaration
  - `internal/server/manager.go` - empty package declaration
  - `internal/client/chat.go` - empty package declaration
  - `internal/ui/window.go` - empty package declaration
- Run `go build` to verify

**Success Criteria:
- Project compiles cleanly

**Dependencies: Step 1.2

---

## Files Created:
- `go.mod`
- `go.sum`
- `main.go`
- `internal/config/config.go` (placeholder)
- `internal/server/manager.go` (placeholder)
- `internal/client/chat.go` (placeholder)
- `internal/ui/window.go` (placeholder)
- `internal/ui/settings.go` (placeholder)

## Dependencies on other phases:
- None

## Review Notes:
- Fyne v2.5.2 requires Go 1.21+
- On Windows, no extra system dependencies needed for Fyne (native controls)
- On Linux, GTK3 or X11 may be needed
- On macOS, uses native Cocoa
