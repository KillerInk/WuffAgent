# WuffAgent - Build Instructions

## Prerequisites

- Rust toolchain (Rust 1.75+): https://www.rust-lang.org/tools/install
- `cargo` must be on PATH
- (Optional) Python 3 — only needed for the MCP stdio integration tests; they skip themselves when no interpreter is available.

## Project Structure

This project is a Cargo workspace with two crates:

| Crate | Purpose |
|-------|---------|
| `wuffagent-core` | Shared library: types, LLM client (HTTP/SSE), config, server, sessions, tools (builtin + plugins + MCP), agents, memory, usage, trimming |
| `wuffagent-egui` | Egui 0.36 frontend binary (egui_plot 0.37 for the usage chart) |

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

> **Windows note:** if WuffAgent is running, `cargo build` fails with
> `failed to remove file ...wuffagent-egui.exe (os error 5)` when relinking.
> Close the app before rebuilding.

## Running

```bash
# Run the egui frontend (default)
cargo run -p wuffagent-egui

# Or from release builds:
target/release/wuffagent-egui.exe
```

## Build Notes

- **Optimized debug builds** — the workspace sets `[profile.dev] opt-level = 1`
  (`debug = "line-tables-only"`) because egui re-runs the whole UI closure on
  every repaint; a plain `opt-level = 0` debug build feels 10–50× slower on
  window drag / scroll / typing even when idle. Keep it.
- **MCP tests** — `cargo test -p wuffagent-core mcp` runs end-to-end tests
  against a small mock MCP server spawned as a python process over stdio.
  Without python the tests skip (and pass) automatically.
- **Self-restart on Windows** — the app cannot relink its own running exe.
  The `restart` tool therefore builds into a separate target dir, e.g.
  `cargo build --target-dir target/relaunch`, and relaunches
  `target\relaunch\debug\wuffagent-egui.exe`.

## Where the app stores data

All app data lives under the user home directory `~/.wuffagent/`:
`config.json` (connection, presets, chat, memory, `mcp_servers`),
`sessions/`, `agents/` (+ `agents/history/` prompt snapshots),
`memories/<project>.json`, `usage.jsonl` (per-call token log), and the
one-shot `restart.json` marker. Native tool plugins are the exception — they
load from the platform config dir, `<platform config dir>/wuffagent/plugins`
(e.g. `%APPDATA%\wuffagent\plugins` on Windows).

## Architecture

```
Cargo.toml (workspace)
├── wuffagent-core/          # Shared backend logic
│   ├── types.rs             # Base types: Message, AppEvent, ReasoningEffort, ...
│   ├── llm.rs               # LlmClient trait + ChatClientAdapter
│   ├── client/              # ChatClient: HTTP/SSE streaming, shared store, overflow retry
│   ├── config/              # Config, presets, encryption, paths, mcp config, search
│   ├── server/              # ServerManager (local server lifecycle)
│   ├── sessions/            # Session persistence (plain or encrypted) + runtime
│   ├── tools/               # ToolManager, ToolRegistry
│   │   ├── builtin/         # file I/O, shell, handoff, restart, memory, web, calc, time
│   │   ├── dynamic/         # native plugin loading (Tool trait ABI)
│   │   └── mcp/             # MCP client (stdio + HTTP, hand-rolled JSON-RPC)
│   ├── agents/              # AgentEngine, Agent, AgentConfig, AgentManager
│   ├── memory/              # MemoryManager: store, search, maintenance, self-improvement
│   ├── usage/               # UsageRecorder (JSONL) + stats (bucketing)
│   └── trimming/            # Conversation trimming: classifier, summarizer
└── wuffagent-egui/          # Egui frontend
    ├── main.rs              # Bootstraps core + runs egui app
    ├── image_loader.rs      # egui image loader for pasted/attached images
    └── src/ui/              # chat, sessions, memory, mcp, usage, improvements, ...
```

The egui frontend shares the `wuffagent-core` backend. The UI lives in
`wuffagent-egui/src/ui/`.

## Agent Execution Model

`wuffagent-core` has a single agent execution core:

- **Chat path** — `AgentEngine::execute_with_tools(request, system_prompt,
  tool_policy, image, cancel)` runs the native tool-calling loop for the
  profile selected in the UI (its system prompt, tool allowlist, shell
  config, reasoning effort, and handoff/restart gates). Chat runs with no
  task timeout.

The old planner → supervisor → workers "plan mode" (and the `/plan` command)
was removed; there is no separate planning pipeline anymore.

`handoff` ends the current agent's turn and continues the same session with
another profile (targets resolved from the same agent-profile discovery dirs
the UI uses). `restart` optionally builds, relaunches the binary, and the
new process auto-resumes the session from a `restart.json` marker.

Every completed LLM call is logged as one JSON line to
`~/.wuffagent/usage.jsonl` (best-effort; a log failure never breaks the
chat) and feeds the egui usage panel's hour/day/week charts.

## Building a Single Crate

```bash
# Build only the core library
cargo build -p wuffagent-core

# Build only the egui binary
cargo build -p wuffagent-egui
```
