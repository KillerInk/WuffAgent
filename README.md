# WuffAgent

A desktop AI agent client written in Rust. WuffAgent connects to local (llama.cpp-style) or remote (OpenAI-compatible) LLM servers, runs multi-agent workflows with tool use, and provides an egui-based GUI for chatting, sessions, and configuration.

## Features

- **Local & remote LLM backends** — connect to a local `llama-server` (or similar) or any remote OpenAI-compatible API. Connection presets can be saved and switched in the UI.
- **Multi-agent system** — agents are defined as JSON files in the `agents/` directory. An `AgentEngine` routes requests to the best-matching agent and runs its LLM tool loop.
- **Plan mode** — the `/plan <request>` command runs a planner → supervisor → workers pipeline. Each planned task is executed through the same `AgentEngine`, so both modes share one execution core.
- **Tool calling** — built-in tools for file I/O, shell execution, calculation, time, web search, and agent-to-agent invocation. Native plugins can be loaded from `<config_dir>/wuffagent/plugins` via the `Tool` trait ABI.
- **Sessions** — conversation sessions are persisted as JSON (with optional ChaCha20Poly1305 encryption) and browsable from the sessions panel.
- **Project memory** — per-project memory files are extracted, improved, and searched across conversations.
- **Conversation trimming** — classifier + summarizer keep long conversations within context limits.
- **egui frontend** — chat area, agent chain panel, presets dialog, settings, and status bar, all in a single-window desktop app.

## Project Structure

```
Cargo.toml (workspace)
├── wuffagent-core/          # Shared backend library (crate: wuffagent_core)
│   └── src/
│       ├── types.rs         # Base types: Message, AppEvent, ReasoningEffort, ...
│       ├── llm.rs           # LlmClient trait + ChatClientAdapter
│       ├── client/          # ChatClient: HTTP/SSE streaming
│       ├── config/          # Config, connection presets, encryption, paths
│       ├── server/          # ServerManager (local server lifecycle)
│       ├── sessions/        # Session persistence (plain or encrypted)
│       ├── tools/           # ToolManager, ToolRegistry, builtin + dynamic tools
│       ├── agents/          # AgentConfig, AgentEngine, AgentRegistry
│       ├── memory/          # MemoryManager: extraction, improvement, search
│       └── trimming/        # Conversation trimming: classifier, summarizer
└── wuffagent-egui/          # egui/eframe frontend binary
    └── src/
        ├── main.rs          # Bootstraps core + runs the egui app
        └── ui/              # Chat, sessions, settings, presets, theme, ...
```

Module dependency rules are documented in [`wuffagent-core/src/lib.rs`](wuffagent-core/src/lib.rs): `types` is dependency-free, and there are no circular dependencies between top-level modules.

## Prerequisites

- Rust toolchain (Rust 1.75+) with `cargo` on PATH — https://www.rust-lang.org/tools/install

## Building & Running

```bash
cargo check                 # quick compile check
cargo test                  # run all tests
cargo build                 # build all crates (debug)
cargo build --release       # build for release

cargo run -p wuffagent-egui # run the GUI
```

Or run the built binary directly (Windows: `target/release/wuffagent-egui.exe`).

## Configuration

On first run WuffAgent creates a config next to the executable (portable-app style); otherwise it falls back to the platform config dir (e.g. `%APPDATA%\wuffagent\` on Windows).

| File | Purpose |
|------|---------|
| `config.json` | Connection settings, model, presets, chat options |
| `agents/<name>.json` | One agent config per file (name, description, personality, allowed tools, priorities) |
| `sessions/` | Persisted conversation sessions |
| `plugins/` | Native tool plugins (`.dll`/`.so`/`.dylib`) |
| `memories/projects/<project>.json` | Per-project memory |

A sample agent config:

```json
{
  "name": "generalist",
  "description": "General purpose worker for tasks that don't fit other categories",
  "personality": "You are a versatile generalist worker...",
  "allowed_tools": ["file_io", "calculation", "web_search", "time", "agent_call"],
  "priority": 10,
  "max_concurrent": 4,
  "can_invoke": ["researcher", "coder", "executor"],
  "handoff_enabled": true
}
```

Agent discovery order: config-dir `agents/`, then `<exe>/../agents`, then `./agents`. First-seen name wins.

## Development Notes

- See [AGENTS.md](AGENTS.md) for agent-oriented guidance and [BUILD.md](BUILD.md) for build details.
- Tests are inline `#[cfg(test)]` modules; run a single test with `cargo test -p wuffagent-core <test_name>`.
- New persisted JSON files should use the atomic temp-file + rename pattern (see `wuffagent-core/src/memory/storage.rs`).
