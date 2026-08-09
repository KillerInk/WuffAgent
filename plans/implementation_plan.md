# WuffAgent - Phased Implementation Plan (Rust)

## Overview

This plan is designed for **pause/resume** capability. Each phase is self-contained, and you can stop at any point and resume later without losing context.

---

## Phase 1: Project Foundation

### Step 1.1: Initialize Cargo Project

**Objective**: Set up Rust project with eframe/egui dependency.

**Tasks**:
- Run `cargo init wuffagent`
- Create `Cargo.toml` with dependencies:
  - `eframe = "0.30"` (application framework)
  - `egui = "0.30"` (GUI toolkit)
  - `serde = { version = "1.0", features = ["derive"] }`
  - `serde_json = "1.0"`
  - `reqwest = { version = "0.12", features = ["stream"] }`
  - `tokio = { version = "1", features = ["full"] }`
  - `tracing = "0.1"`
  - `dirs = "5.0"`
- Create basic `src/main.rs` with eframe setup

**Success Criteria**:
- `cargo check` succeeds
- `cargo run` opens an empty window

**Dependencies**: None

---

### Step 1.2: Create Directory Structure

**Objective**: Set up src module layout.

**Tasks**:
- Create `src/config/`, `src/server/`, `src/client/`, `src/ui/`
- Create empty `mod.rs` files in each
- Create `src/main.rs` with module declarations
- Verify `cargo check` passes

**Success Criteria**:
- All directories exist
- `cargo check` compiles successfully

**Dependencies**: Step 1.1

---

### Step 1.3: Verify Build

**Objective**: Ensure project builds after directory creation.

**Tasks**:
- Create placeholder files:
  - `src/config/mod.rs` - empty module
  - `src/server/mod.rs` - empty module
  - `src/client/mod.rs` - empty module
  - `src/ui/mod.rs` - empty module
  - `src/ui/window.rs` - stub ChatApp
- Run `cargo build` to verify

**Success Criteria**:
- Project compiles cleanly

**Dependencies**: Step 1.2

---

## Phase 2: Config Management

### Step 2.1: Config Struct and Defaults

**Objective**: Define configuration data structure with serde.

**Tasks**:
- Create `src/config/mod.rs`
- Define `Config` struct with `#[derive(Serialize, Deserialize)]`
- Implement `default_config()` function
- Implement `load()` with JSON deserialization
- Implement `save()` with JSON serialization
- Implement `validate()` with field validation

**Success Criteria**:
- Config loads from file
- `save()` writes valid JSON
- `validate()` catches errors (empty paths, invalid port, etc.)

**Dependencies**: Step 1.1 (module exists)

---

### Step 2.2: Config File Location

**Objective**: Config file location strategy using `dirs`.

**Tasks**:
- Implement `get_config_path()` using `dirs::config_dir()`
- Create `config.json` alongside binary if not in config dir
- Handle platform differences (Windows, macOS, Linux)

**Success Criteria**:
- Config file path is correct on each platform

**Dependencies**: Step 2.1

---

### Step 2.3: Config Persistence

**Objective**: Persistent settings between sessions.

**Tasks**:
- Implement file read/write logic
- Handle file not found gracefully
- Return default config when file missing

**Success Criteria**:
- Config file is created on first run
- Existing config is loaded on subsequent runs

**Dependencies**: Step 2.1

---

## Phase 3: Server Manager

### Step 3.1: Process Lifecycle

**Objective**: Start and stop llama-server process using tokio.

**Tasks**:
- Create `src/server/mod.rs`
- Implement `ServerManager` struct
- `start_server()` spawns `llama-server` with config params via `tokio::process::Command`
- `stop_server()` sends SIGTERM, waits for exit
- `is_running()` checks process state
- `wait_for_ready()` polls HTTP endpoint

**Success Criteria**:
- Server process starts with correct arguments
- Process is running after `start_server()`
- Server stops after `stop_server()`
- `is_running()` returns correct boolean
- `wait_for_ready()` returns true when server responds

**Dependencies**: Step 2.1 (config available)

---

### Step 3.2: Process Monitoring

**Objective**: Detect server crashes, handle errors.

**Tasks**:
- Spawn task to monitor stdout/stderr
- Implement `wait()` to detect process exit
- Log server output to tracing
- Expose error channel for UI updates

**Success Criteria**:
- Crash detection works
- Error messages are visible
- Process state is tracked

**Dependencies**: Step 3.1

---

### Step 3.3: Model Loading Progress

**Objective**: Show loading progress during model loading.

**Tasks**:
- Parse stderr for loading progress messages from llama-server
- Expose progress channel for UI updates
- Extract percentage from output lines

**Success Criteria**:
- Progress percentage available

**Dependencies**: Step 3.1

---

## Phase 4: Chat Client

### Step 4.1: HTTP Request Builder

**Objective**: Build HTTP requests for chat completions.

**Tasks**:
- Create `src/client/mod.rs`
- Define `Message`, `ChatRequest`, `Response` structs with serde
- Implement `build_request()` method
- Serialize to JSON, create HTTP request with reqwest

**Success Criteria**:
- JSON matches OpenAI API format
- Request includes system prompt, message history

**Dependencies**: Step 2.1 (config for base URL)

---

### Step 4.2: Non-Streaming Response Handling

**Objective**: Send request, receive complete response.

**Tasks**:
- Implement `send_message()` with `stream: false`
- Parse JSON response
- Return content to caller
- Update conversation history

**Success Criteria**:
- Complete response text is returned
- Error handling for HTTP failures
- Conversation history is updated

**Dependencies**: Step 4.1

---

### Step 4.3: SSE Streaming

**Objective**: Stream tokens as they arrive.

**Tasks**:
- Implement `stream_message()` with `stream: true`
- Parse SSE `data:` lines using reqwest streaming
- Call callback for each token chunk
- Use async callback pattern
- Support cancellation via tokio abort
- Handle `[DONE]` marker

**Success Criteria**:
- Callback fires for each token
- Streaming completes on `[DONE]` marker
- Can cancel streaming with abort

**Dependencies**: Step 4.1

---

## Phase 5: Main Window

### Step 5.1: App Skeleton

**Objective**: Create basic eframe app with layout.

**Tasks**:
- Create `src/ui/window.rs`
- Create `ChatApp` struct implementing `eframe::App`
- Set window title, size
- Add placeholder content

**Success Criteria**:
- App opens, titled "WuffAgent"
- Content is visible

**Dependencies**: Step 2.1 (config), Step 3.1 (server manager available)

---

### Step 5.2: Chat Display Area

**Objective**: Scrollable chat message history.

**Tasks**:
- Add `egui::ScrollArea` with message list
- Implement `display_message()` to add messages
- Auto-scroll to bottom on new messages

**Success Criteria**:
- Messages display in order
- Scrollbar appears when content overflows
- New messages are visible

**Dependencies**: Step 5.1

---

### Step 5.3: Input Area

**Objective**: User input field and send button.

**Tasks**:
- Add `egui::TextEdit` for input (multi-line)
- Add `egui::Button` for send
- Add `egui::Button` for stop
- Wire send button to submit
- Wire stop button to cancel

**Success Criteria**:
- User can type in input field
- Send button triggers submission
- Stop button is visible during generation

**Dependencies**: Step 5.1

---

### Step 5.4: Status Bar

**Objective**: Show server state (connecting, ready, generating, error).

**Tasks**:
- Add `egui::Label` for status
- Update on server state changes
- Color-code: green=ready, blue=generating, red=error, gray=connecting

**Success Criteria**:
- Status text changes based on state
- Colors are visible

**Dependencies**: Step 5.1

---

### Step 5.5: Bottom Bar

**Objective**: Settings, Clear, and exit buttons.

**Tasks**:
- Add `egui::Button` for settings
- Add `egui::Button` for clear history
- Add `egui::Button` for exit

**Success Criteria**:
- All buttons visible
- Buttons are functional

**Dependencies**: Step 5.2 (chat display exists)

---

## Phase 6: Settings Panel

### Step 6.1: Settings Dialog

**Objective**: Settings dialog with server config fields.

**Tasks**:
- Create `src/ui/settings.rs`
- Add fields for server_path, model_path, port, gpu_layers, ctx_size, threads, system_prompt
- Add save, cancel, start, stop buttons

**Success Criteria**:
- Settings dialog opens
- All fields visible
- Save writes to JSON

**Dependencies**: Step 2.1 (config struct exists)

---

### Step 6.2: File Pickers

**Objective**: Allow browsing for server and model paths.

**Tasks**:
- Add file picker buttons for server_path, model_path
- Validate selected file exists
- Update config fields

**Success Criteria**:
- File picker opens
- Selected paths are saved

**Dependencies**: Step 6.1

---

### Step 6.3: Server Control

**Objective**: Start/stop server from settings.

**Tasks**:
- Wire start/stop buttons to server manager
- Show status in settings
- Validate paths before starting

**Success Criteria**:
- Server starts with selected paths
- Server stops correctly

**Dependencies**: Step 3.1 (server manager)

---

## Phase 7: Integration

### Step 7.1: Wire Config to UI

**Objective**: Load config at startup, save on close.

**Tasks**:
- `main.rs` loads config
- Pass config to app
- Save config on app close

**Success Criteria**:
- Settings persist between runs
- App uses loaded config

**Dependencies**: Step 5.1, Step 6.1

---

### Step 7.2: Wire Client to Server

**Objective**: Chat client connects to running server.

**Tasks**:
- App sends messages via client
- Client uses config for base URL

**Success Criteria**:
- Messages go to server
- Responses come back

**Dependencies**: Step 4.1, Step 5.1

---

### Step 7.3: End-to-End Flow

**Objective**: Complete chat cycle works.

**Tasks**:
- User types message
- Client sends to server
- Response displays in chat
- History updates

**Success Criteria**:
- Full round-trip works
- Chat history updates

**Dependencies**: All previous phases

---

## Phase 8: Streaming Integration

### Step 8.1: Streaming to UI

**Objective**: Streaming responses update UI in real time.

**Tasks**:
- Wire `stream_message` to chat display
- Each token chunk appears immediately
- Progress updates as tokens arrive
- Use `egui::Context::request_repaint()` for responsive UI

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

**Success Criteria**:
- Generation stops when user clicks stop
- History is cleaned up
- UI returns to ready state

**Dependencies**: Step 8.1

---

## Phase 9: Polish

### Step 9.1: Theme Support

**Objective**: Light/dark theme toggle.

**Tasks**:
- Add theme button to UI
- Toggle egui theme
- Persist preference in config

**Success Criteria**:
- Theme changes are visible
- Theme persists between sessions

**Dependencies**: Step 5.1

---

### Step 9.2: Error Handling

**Objective**: Robust error recovery.

**Tasks**:
- Show error dialogs for failures
- Handle server disconnects
- Auto-reconnect if server drops

**Success Criteria**:
- Errors are caught
- User is informed
- Recovery is possible

**Dependencies**: All phases

---

### Step 9.3: Context Persistence

**Objective**: Save chat history between sessions.

**Tasks**:
- Add chat history to config
- Save on close, load on startup

**Success Criteria**:
- Chat history survives restarts

**Dependencies**: Step 2.1 (config), Step 5.1 (UI)

---

### Step 9.4: Input Validation

**Objective**: Validate user input.

**Tasks**:
- Empty message check
- Max message length check

**Success Criteria**:
- Invalid input is rejected

**Dependencies**: Step 5.3

---

### Step 9.5: Model Info Display

**Objective**: Show model name, context size, GPU layers.

**Tasks**:
- Parse server startup logs for model info
- Display in status bar

**Success Criteria**:
- Model info visible

**Dependencies**: Step 3.1, Step 5.4

---

## Phase 10: Testing

### Step 10.1: Unit Tests

**Objective**: Core logic tests.

**Tasks**:
- Test config load/save
- Test request building
- Test message parsing
- Test history truncation

**Success Criteria**:
- `cargo test` passes
- All tests pass

**Dependencies**: Steps 2.1, 4.1

---

### Step 10.2: Integration Tests

**Objective**: Server manager and client tests.

**Tasks**:
- Test server start/stop cycle
- Test chat flow with mock server

**Success Criteria**:
- Process management works
- API calls are correct

**Dependencies**: Step 3.1, Step 4.1

---

### Step 10.3: Final Build

**Objective**: Compile final binary.

**Tasks**:
- Build with `cargo build --release`
- Test on Windows
- Verify single executable works

**Success Criteria**:
- Binary runs on Windows
- No external runtime needed
- Single executable works

**Dependencies**: All phases

---

## Files Created

| Phase | Files |
|-------|-------|
| 1 | `Cargo.toml`, `src/main.rs`, `src/config/mod.rs`, `src/server/mod.rs`, `src/client/mod.rs`, `src/ui/mod.rs`, `src/ui/window.rs` |
| 2 | `src/config/mod.rs` (full) |
| 3 | `src/server/mod.rs` (full) |
| 4 | `src/client/mod.rs` (full) |
| 5 | `src/ui/window.rs` (full) |
| 6 | `src/ui/settings.rs` |
| 7 | `src/main.rs` (full), `src/ui/window.rs` |
| 8 | `src/client/mod.rs`, `src/ui/window.rs` |
| 9 | `src/ui/*.rs`, `src/config/mod.rs` |
| 10 | `*_test.rs`, final binary |
