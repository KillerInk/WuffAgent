# WuffAgent - Implementation Plans

## Overview

This directory contains the phased implementation plan for WuffAgent, split by phase for pause/resume capability.

---

## Plan Files

| Phase | File | Description |
|-------|------|-------------|
|  | [`phase01_project_foundation.md`](phase01_project_foundation.md) | Go module, directory structure |
|  | [`phase02_config_management.md`](phase02_config_management.md) | Config struct, persistence |
|  |[`phase03_server_manager.md`](phase03_server_manager.md) | llama-server process management |
|  |[`phase04_chat_client.md`](phase04_chat_client.md) | HTTP client, SSE streaming |
|  |[`phase05_main_window.md`](phase05_main_window.md) | Fyne UI, chat display, input, status |
|  |[`phase06_settings_panel.md`](phase06_settings_panel.md) | Settings dialog, file pickers |
|  |[`phase07_integration.md`](phase07_integration.md) | Wire everything together |
|  |[`phase08_streaming.md`](phase08_streaming.md) | Streaming integration, stop generation |
|  |[`phase09_polish.md`](phase09_polish.md) | Theme, errors, persistence |
|  |[`phase10_testing.md`](phase10_testing.md) | Unit tests, integration tests, final build |

---

## Architecture

See [`architecture.md`](architecture.md) for the overall design.

---

## Dependency Graph

```mermaid
graph TB
    subgraph Phase1
        A[1.1 Module]
        A-->B[1.2 Structure]
        B-->C[1.3 Verify Build]
    end
    
    subgraph Phase2
        C-->D[2.1 Config Struct]
        D-->E[2.2 Config Location]
        E-->F[2.3 Config Persistence]
    end
    
    subgraph Phase3
        F-->G[3.1 Process Lifecycle]
        G-->H[3.2 Process Monitoring]
        H-->I[3.3 Loading Progress]
    end
    
    subgraph Phase4
        D-->J[4.1 Request Builder]
        J-->K[4.2 Non-Streaming]
        J-->L[4.3 SSE Streaming]
        L-->M[4.4 Context Window]
        L-->N[4.5 Stop Generation]
        L-->O[4.6 Thread Safety]
    end
    
    subgraph Phase5
        C-->P[5.1 Window Skeleton]
        P-->Q[5.2 Chat Display]
        P-->R[5.3 Input Area]
        P-->S[5.4 Status Bar]
        P-->T[5.5 Bottom Bar]
        P-->U[5.6 Loading Progress]
        P-->V[5.7 Keyboard Shortcuts]
    end
    
    subgraph Phase6
        D-->W[6.1 Settings Window]
        W-->X[6.2 File Pickers]
        W-->Y[6.3 Server Control]
    end
    
    subgraph Phase7
        P-->Z[7.1 Config to UI]
        K-->AA[7.2 Client to Server]
        AA-->AB[7.3 End-to-End Flow]
    end
    
    subgraph Phase8
        L-->AC[8.1 Streaming to UI]
        AC-->AD[8.2 Streaming Toggle]
        N-->AE[8.3 Stop Implementation]
    end
    
    subgraph Phase9
        P-->AF[9.1 Theme]
        W-->AG[9.2 Error Handling]
        D-->AH[9.3 Context Persistence]
        R-->AI[9.4 Input Validation]
        P-->AJ[9.5 Model Info Display]
    end
    
    subgraph Phase10
        D-->AK[10.1 Unit Tests]
        AK-->AL[10.2 Integration Tests]
        AL-->AM[10.3 Final Build]
    end
```

---

## Pause/Resume Guide

At any phase, run `go build` to verify compilation. If it passes, you can stop here.

To resume, check the last completed phase and start the next one.

---

## Quick Reference

| Phase | Files | Key Functions |
|-------|-------|---------------|
|  | go.mod, main.go | `go mod init`, `app.New()` |
|  | internal/config/config.go | `Config`, `LoadConfig`, `Save`, `Validate` |
|  | internal/server/manager.go | `ServerManager`, `StartServer`, `StopServer` |
|  | internal/client/chat.go | `ChatClient`, `SendMessage`, `StreamMessage`, `StopGeneration` |
|  | internal/ui/window.go | `NewChatWindow`, `DisplayMessage`, `StreamUpdate` |
|  | internal/ui/settings.go | `SettingsDialog`, `SaveConfig`, `StartServer` |
|  | main.go | `main()`, full app wiring |
|  | internal/client/chat.go, internal/ui/window.go | `StreamMessage` to UI updates |
|  | internal/ui/*.go | Theme, errors, persistence |
|  | *_test.go | Tests, build, release |

---

## Go Modules Required

```
module wuffagent

go 1.21

require github.com/fyne-io/fyne/v2 v2.5.2
```

---

## Critical Design Notes

1. **Thread Safety**: All UI updates from goroutines MUST use `app.CurrentApp().CallLater()` for Fyne widget updates.
2. **Stop Generation**: No abort endpoint in llama-server. Stop works by closing HTTP response body.
3. **SSE Parsing**: Handle partial lines, JSON per line, `[DONE]` marker.
4. **Context Window**: History truncation keeps system prompt, removes oldest messages.
4. **Config Location**: Config file is next to binary via `os.Executable()`.

---

## Review Improvements Applied

From the comprehensive review, the following critical issues were addressed:

1. **Stop Generation**: Added Step 8.3 with HTTP response closure and history cleanup
2. **Model Loading Progress**: Added Step 5.6 for loading progress indicator
3. **Thread Safety**: Added Step 4.6 for Fyne `CallLater()` documentation
4. **SSE Parsing**: Expanded Step 4.3 with detailed parsing logic
5. **Context Window**: Added Step 4.4 for history management
6. **Input Validation**: Added Step 9.4 for input validation
7. **Model Info Display**: Added Step 9.5 for model information
8. **Keyboard Shortcuts**: Added Step 5.7 for Ctrl+Enter, Escape
9. **Config Location**: Added Step 2.2 for binary-relative config path
10. **Error Handling**: Enhanced Step 9.2 with auto-reconnect

---

## Files per Phase

| Phase | Files Created/Modified |
|-------|-------------------|
|  | go.mod, go.sum, main.go |
|  | internal/config/config.go |
|  | internal/server/manager.go |
|  | internal/client/chat.go |
|  | internal/ui/window.go |
|  | internal/ui/settings.go |
|  | main.go |
|  | internal/client/chat.go, internal/ui/window.go |
|  | internal/ui/*.go, config.json |
|  | *_test.go, final binary |

---

## Next Steps

1. Review this plan
2. Start with Phase 1
3. Follow the dependency graph
