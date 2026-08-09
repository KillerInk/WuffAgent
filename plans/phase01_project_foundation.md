# Phase 1: Project Foundation (Rust)

## Status: Pending

---

### Step 1.1: Initialize Cargo Project

**Objective**: Set up Rust project with eframe/egui dependency.

**Tasks**:
- Run `cargo init wuffagent`
- Create `Cargo.toml` with dependencies:

```toml
[package]
name = "wuffagent"
version = "0.1.0"
edition = "2021"

[dependencies]
eframe = "0.30"
egui = "0.30"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
reqwest = { version = "0.12", features = ["stream"] }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dirs = "5.0"
```

- Create `src/main.rs` with eframe setup:

```rust
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::AppOptions {
        viewport: egui::ViewportBuilder::default().with_size(900, 700),
        ..Default::default()
    };
    eframe::run_native(
        "WuffAgent",
        options,
        Box::new(|cc| Ok(Box::new(ChatApp::new(cc)))),
    )
}
```

**Success Criteria**:
- `cargo check` succeeds
- `cargo run` opens an empty window

**Dependencies**: None

---

### Step 1.2: Create Directory Structure

**Objective**: Set up src module layout with proper Rust structure.

**Tasks**:
- Create `src/config/`, `src/server/`, `src/client/`, `src/ui/`
- Create empty `mod.rs` files in each package
- Update `src/main.rs` with module declarations

**Directory structure**:

```
WuffAgent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── config/
│   │   └── mod.rs
│   ├── client/
│   │   └── mod.rs
│   ├── server/
│   │   └── mod.rs
│   └── ui/
│       ├── mod.rs
│       ├── window.rs
│       └── settings.rs
```

**Success Criteria**:
- All directories exist
- `cargo check` compiles successfully

**Dependencies**: Step 1.1

---

### Step 1.3: Verify Build

**Objective**: Ensure project builds after directory creation.

**Tasks**:
- Create placeholder files:
  - `src/config/mod.rs` - empty module declaration
  - `src/server/mod.rs` - empty module declaration
  - `src/client/mod.rs` - empty module declaration
  - `src/ui/mod.rs` - module declarations for window and settings
  - `src/ui/window.rs` - stub ChatApp struct
  - `src/ui/settings.rs` - stub SettingsDialog struct
- Run `cargo build` to verify

**Success Criteria**:
- Project compiles cleanly

**Dependencies**: Step 1.2

---

## Files Created:
- `Cargo.toml`
- `src/main.rs`
- `src/config/mod.rs` (placeholder)
- `src/server/mod.rs` (placeholder)
- `src/client/mod.rs` (placeholder)
- `src/ui/mod.rs` (placeholder)
- `src/ui/window.rs` (placeholder)
- `src/ui/settings.rs` (placeholder)

## Dependencies on other phases:
- None

## Review Notes:
- eframe 0.30 requires Rust 2021 edition
- On Windows, no extra system dependencies needed for eframe (uses winit)
- On Linux, GTK3 or X11 may be needed
- On macOS, uses native Cocoa
- `tokio::full` features are needed for async process management and HTTP
- `reqwest` with `stream` feature is needed for SSE streaming
- `dirs` crate is used for platform-appropriate config file locations
