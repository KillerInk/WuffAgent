# WuffAgent

A desktop AI agent client written in Rust. WuffAgent connects to local (llama.cpp-style) or remote (OpenAI-compatible) LLM servers, runs multi-agent workflows with tool use, and provides an egui-based GUI for chatting, sessions, and configuration.

## Features

- **Local & remote LLM backends** — connect to a local `llama-server` (or similar) or any remote OpenAI-compatible API. Connection presets can be saved and switched in the UI.
- **Multi-agent system** — agents are defined as JSON files in the `agents/` directory. An `AgentEngine` routes requests to the best-matching agent and runs its LLM tool loop.
- **Plan mode** — the `/plan <request>` command runs a planner → supervisor → workers pipeline. Each planned task is executed through the same `AgentEngine`, so both modes share one execution core.
- **Tool calling** — built-in tools for file I/O, shell execution, calculation, time, web search, and agent-to-agent invocation. Native plugins can be loaded from `<config_dir>/wuffagent/plugins` via the `Tool` trait ABI. Inside an agent, a tool call is started in the background as soon as the model finishes emitting it (while the model keeps streaming/reasoning), so tool execution overlaps with model inference; results are still recorded in call order.
- **Sessions** — conversation sessions are persisted as JSON (with optional ChaCha20Poly1305 encryption) and browsable from the sessions panel.
- **Project memory** — per-project memory stores written by agent tools (`save_memory`, `update_memory`, `consolidate_memories`, `delete_memory`), with keyword search, query-aware injection into system prompts, and an opt-in LLM maintenance pass (merge/update/delete). Managed from a dedicated memory panel (list, search, edit, delete, run maintenance).
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
│       ├── memory/          # MemoryManager: tool-based store, search, maintenance, self-improvement
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
| `memories/<project>.json` | Per-project memory |

A sample agent config:

```json
{
  "name": "generalist",
  "description": "General purpose worker for tasks that don't fit other categories",
  "personality": "You are a versatile generalist worker...",
  "allowed_tools": ["read_file", "write_file", "apply_diff", "list_dir", "search_files", "search_content", "mkdir", "delete", "copy", "move", "file_info", "calculation", "web_search", "time", "agent_call"],
  "priority": 10,
  "max_concurrent": 4,
  "can_invoke": ["researcher", "coder", "executor"],
  "handoff_enabled": true
}
```

Agent discovery order: config-dir `agents/`, then `<exe>/../agents`, then `./agents`. First-seen name wins.

## Memory

Memory is a per-project JSON store (default `~/.wuffagent/memories/<project>.json`) managed entirely through the [`MemoryManager`](wuffagent-core/src/memory/manager.rs). There is no background batch extraction — agents are the only write path:

- **Tools** — `save_memory`, `update_memory`, `consolidate_memories`, and `delete_memory` let agents create, revise, merge, and prune entries. `save_memory` returns the new entry ID plus the top similar existing entries so the agent can update or consolidate instead of duplicating.
- **Dedup gate** — `MemoryManager::add` runs a keyword search first; strong matches return the existing entry (id + content) rather than inserting a near-duplicate.
- **Injection** — relevant memories are selected by the user's query (`search`) and injected into the agent system prompt; with no hits, Always mode falls back to the most recent active entries. `get_recent` skips superseded/expired entries.
- **Maintenance (opt-in)** — with `memory_maintenance` enabled, `run_maintenance()` sends all entries to the LLM and applies its `merge`/`update`/`delete` actions (unknown ids skipped, never wipes everything). It auto-triggers after a task only when enabled, above `memory_maintenance_threshold`, and past the cooldown.
- **Self-improvement** — when `auto_improve` is on, the engine calls `suggest_improvements` after a task and emits `ImprovementSuggested`; approving in the UI writes the updated agent JSON (or creates a new agent) and refreshes the registry.
- **UI** — the memory panel (🧠) lists all entries, searches, edits content/tags, deletes (with confirm), and runs maintenance on demand; a memory settings section exposes enabled, injection mode, maintenance toggle/threshold, and the auto-improve toggle. The status bar shows an active-memory count with a tooltip.

## Web search

`web_search` (`{query, max_results?}` → `{query, results: [{title, url, snippet}]}`) and `fetch_url` (`{url, max_bytes?}` → page content as plain text) let agents research the web.

- **Backends** — `Auto` (default: Bing → Yahoo → DuckDuckGo, automatic failover on error/empty results), `Bing`, `Yahoo`, `DuckDuckGo`, `SearXNG` (self-hosted, needs a base URL). Configured in the `search` block of `wuffagent.json` or in Settings → **Web search**; backend changes take effect on restart.
- **Env overrides** (headless use; take precedence over config) — `WUFFAGENT_SEARCH_BACKEND=auto|bing|yahoo|duckduckgo` and `WUFFAGENT_SEARXNG_URL=https://…` (forces the SearXNG backend).
- **Caching** — results are cached in memory with a TTL (`search.cache_duration_secs`, default 300 s), so repeated queries within the window cost no HTTP requests.

## Development Notes

- See [AGENTS.md](AGENTS.md) for agent-oriented guidance and [BUILD.md](BUILD.md) for build details.
- Tests are inline `#[cfg(test)]` modules; run a single test with `cargo test -p wuffagent-core <test_name>`.
- New persisted JSON files should use the atomic temp-file + rename pattern (see `wuffagent-core/src/memory/storage.rs`).
