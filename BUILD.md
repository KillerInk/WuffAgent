# WuffAgent - Build Instructions

## Prerequisites

- Rust toolchain (Rust 1.75+): https://www.rust-lang.org/tools/install
- `cargo` must be on PATH

## Project Structure

This project is a Cargo workspace with two crates:

| Crate | Purpose |
|-------|---------|
| `wuffagent-core` | Shared library: types, client, config, server, sessions, tools, agents |
| `wuffagent-egui` | Egui-based frontend binary |

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

# Or from release builds:
target/release/wuffagent-egui.exe
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
└── wuffagent-egui/          # Egui frontend
    └── src/main.rs          # Bootstraps core + runs egui app
```

The egui frontend shares the `wuffagent-core` backend. The UI lives in
`wuffagent-egui/src/ui/`.

## Agent Execution Model

`wuffagent-core` has a single agent execution core with two modes:

- **Simple routing path** — `AgentEngine::execute` routes the request to the
  best-matching agent and runs its LLM tool loop.
- **Plan path** — `AgentEngine::execute_plan_mode` runs the planner →
  supervisor → workers pipeline. Each planned task is executed *through* the
  `AgentEngine` via the `EngineWorker` bridge (a `WorkerAgent` that delegates to
  the engine), so both modes share one execution core.

The egui `/plan <request>` command uses the plan path. The `LlmClientAdapter`
bridges the engine's `LlmClient` to the `ChatClientLike` trait the planner needs.

## Building a Single Crate

```bash
# Build only the core library
cargo build -p wuffagent-core

# Build only the egui binary
cargo build -p wuffagent-egui
```
