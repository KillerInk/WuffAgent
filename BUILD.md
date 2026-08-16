# WuffAgent - Build Instructions

## Prerequisites

- Rust toolchain (Rust 1.75+): https://www.rust-lang.org/tools/install
- `cargo` must be on PATH

## Project Structure

This project is a Cargo workspace with three crates:

| Crate | Purpose |
|-------|---------|
| `wuffagent-core` | Shared library: types, client, config, server, sessions, tools, agents |
| `wuffagent-egui` | Egui-based frontend binary |
| `wuffagent-iced` | Iced-based frontend binary |

## Quick Start

```bash
# Clone the repository
git clone <repo-url>
cd WuffAgent

# Run checks and tests
cargo check
cargo test

# Build all crates
cargo build

# Build for release
cargo build --release
```

## Running

```bash
# Run the egui frontend (default)
cargo run -p wuffagent-egui

# Run the iced frontend
cargo run -p wuffagent-iced

# Or from release builds:
target/release/wuffagent-egui.exe
target/release/wuffagent-iced.exe
```

## Architecture

```
Cargo.toml (workspace)
├── wuffagent-core/          # Shared backend logic
│   ├── types.rs
│   ├── client/
│   ├── config/
│   ├── server/
│   ├── sessions/
│   ├── tools/
│   └── agents/
├── wuffagent-egui/          # Egui frontend
│   └── src/main.rs          # Bootstraps core + runs egui app
└── wuffagent-iced/          # Iced frontend
    ├── wuffagent-iced-app/  # Shared iced UI logic
    └── src/main.rs          # Bootstraps core + runs iced app
```

Both frontends share the same `wuffagent-core` backend. The egui UI lives in
`wuffagent-egui/src/ui/` and the iced UI lives in `wuffagent-iced-app/src/app/`.

## Building a Single Crate

```bash
# Build only the core library
cargo build -p wuffagent-core

# Build only the egui binary
cargo build -p wuffagent-egui

# Build only the iced binary
cargo build -p wuffagent-iced
```
