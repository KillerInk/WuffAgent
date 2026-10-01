# WuffAgent

A desktop AI agent client written in Rust. WuffAgent connects to local (llama.cpp-style) or remote (OpenAI-compatible) LLM servers, runs agent profiles with tool use (including external MCP servers), and provides an egui-based GUI for chatting, sessions, memory, skills, run-metrics, and configuration.

## Features

- **Local & remote LLM backends** — connect to a local `llama-server` (or similar) or any remote OpenAI-compatible API. Connection presets can be saved and switched in the UI.
- **Agent profiles** — agents are defined as JSON files in the `agents/` directory (repo or user config dir). An `AgentEngine` runs each profile's LLM tool loop; the chat UI lets you pick a profile (system prompt, tool policy, shell allowlist, reasoning effort, handoff targets) per message. Profiles are self-editable through the `list_agents` / `edit_agent_profile` tools, with every edit snapshotted to a prompt-history directory so changes are revertible.
- **Tool calling** — built-in tools for file I/O, shell execution, calculation, time, web search/fetch, project memory, skills, and image loading (`show_image`), plus the self-improvement tools (`read_metrics`, evals, on-demand improvement). `handoff` lets an agent end its turn and hand the session to another profile; `hand_back` returns a sub-session to its caller; `restart` lets an agent rebuild and relaunch WuffAgent and resume the same session; `session_note` lets an agent pin a short state note that survives trims and reloads. Native plugins can be loaded from `<platform config dir>/wuffagent/plugins` via the `Tool` trait ABI. Inside an agent, a tool call is started in the background as soon as the model finishes emitting it (while the model keeps streaming/reasoning), so tool execution overlaps with model inference; results are still recorded in call order.
- **MCP (Model Context Protocol) servers** — connect to external MCP servers over stdio (child process) or Streamable HTTP, discover their tools via `tools/list`, and expose them to agents as `mcp__<server>__<tool>` tools. Managed from a dedicated MCP panel (add/edit/connect/disable, per-tool allowlist) and the `mcp_*` tools.
- **Sessions** — conversation sessions are persisted as JSON (with optional ChaCha20Poly1305 encryption) and browsable from the sessions panel. `handoff` with `sub_session: true` starts a fresh sub-session (own context, own tab) that can later return with `hand_back`.
- **Project memory** — per-project memory stores written by agent tools (`save_memory`, `update_memory`, `consolidate_memories`, `delete_memory`), with keyword search, query-aware injection into system prompts, and an opt-in LLM maintenance pass (merge/update/delete). Managed from a dedicated memory panel (list, search, edit, delete, run maintenance).
- **Skills (procedural memory)** — reusable named procedures saved by agents (`save_skill`, `list_skills`, `read_skill`, `delete_skill`), stored as markdown files under `~/.wuffagent/skills/`. Each skill's short frontmatter (name, description, when-to-use) is injected into agent system prompts so the model knows what exists; the full body is fetched on demand with `read_skill`.
- **Self-improvement** — with `auto_improve` on (default), the engine runs a throttled post-task LLM check over the agent's lesson/outcome/feedback memories and can suggest prompt/tool-allowlist changes; approving in the improvements panel writes the updated agent JSON (or creates a new agent) and refreshes the registry. An agent can also trigger an on-demand check (`run_self_improvement`) or inspect the loop's state (`list_improvement_status`). Every agent edit is snapshotted to a prompt-history directory so changes are revertible.
- **Run-metrics & cost tracking** — one append-only JSONL file per agent (`~/.wuffagent/metrics/<agent>.jsonl`) records, per run, the tool calls, tool errors, verification outcome, duration, tokens, and estimated USD cost. The same store also logs user feedback, skill use, trim decisions, self-improvement checks, and eval results. Exposed to agents as the `read_metrics` tool and to the UI as a floating fleet dashboard (per-agent KPI cards, a 30-day trend chart, and an improvement-loop state table). Old days are rolled up and pruned by a configurable retention window.
- **Golden/regression evals** — saved per-agent tasks with verification criteria (`save_eval`, `list_evals`, `delete_eval`). `run_eval` executes an agent's evals headlessly on fresh, isolated agents and grades each result against its `expect` criteria with the verification judge; the pass/fail + cost is recorded as an `Eval` metrics line, giving a before/after regression signal after prompt/profile changes.
- **Token usage tracking** — one JSONL line per completed LLM call (model, agent, session, prompt/completion/total tokens) is appended to `~/.wuffagent/usage.jsonl`; a floating usage panel charts input/output/total tokens per hour/day/week with incremental log reads.
- **Image input & output** — paste (Ctrl/Cmd+V) or attach images (PNG/JPEG/GIF/BMP/WebP) in the chat; they are sent to the model as data-URIs on the user message and rendered in the chat bubbles. Agents can also display images themselves with `show_image` (from a file path, data-URI, or URL); the image is downscaled and rendered inside the tool card.
- **Conversation trimming** — classifier + summarizer keep long conversations within context limits.
- **egui frontend** — chat area (with per-message thumbs up/down feedback), agent selector/chain, sessions panel, memory panel, skills, MCP panel, usage panel, fleet dashboard, improvements panel, agent config editor, presets dialog, settings, and status bar, all in a single-window desktop app.

## Project Structure

```
Cargo.toml (workspace)
├── wuffagent-core/          # Shared backend library (crate: wuffagent_core)
│   ├── src/
│   │   ├── types/           # Base types (Message, AppEvent, QueuedMessage, ChatToolPolicy,
│   │   │                    #   ReasoningEffort, policy, events, chat_ui, ...) — dependency-free
│   │   ├── llm.rs           # LlmClient trait + ChatClientAdapter
│   │   ├── activity.rs      # ActivityTracker / ActivityHandle (RAII) + LabeledLlm
│   │   ├── client/          # ChatClient: HTTP/SSE streaming, shared store, overflow retry
│   │   ├── config/          # Config, connection presets, ModelPrice, encryption, paths, mcp, search
│   │   ├── server/          # ServerManager (local server lifecycle)
│   │   ├── sessions/        # Session persistence (plain/encrypted) + per-session state/runtime
│   │   ├── tools/           # ToolManager, ToolRegistry (registry/, manager/), validation, preview
│   │   │   ├── builtin/     # file I/O, shell, handoff/restart/hand_back, memory, web, calc, time,
│   │   │   │                #   image (show_image), skills, session_note, agent_profile, improvement/
│   │   │   ├── dynamic/     # native plugin loading (libloading, Tool trait ABI)
│   │   │   └── mcp/         # MCP client: stdio + HTTP transports, hand-rolled JSON-RPC
│   │   ├── agents/          # Agent, AgentEngine, ChatPipeline, AgentManager, improvement loop,
│   │   │   │                #   metrics/ (run-metrics store), config, types, traits, engine, manager
│   │   ├── memory/          # MemoryManager, search, maintenance, evals/ (EvalStore), skills/ (SkillStore)
│   │   ├── usage/           # UsageRecorder (JSONL) + cost (ModelPrice → $) + stats (bucketing)
│   │   ├── stats/           # Pure time-bucketing shared by usage + metrics (Granularity)
│   │   ├── util/            # Small shared text helpers
│   │   └── trimming/        # Conversation trimming: classifier, summarizer
│   └── tests/fixtures/      # Committed HTML snapshots for offline web-search parser tests
└── wuffagent-egui/          # egui/eframe 0.36 frontend binary
    └── src/
        ├── main.rs          # Bootstraps core + runs the egui app
        ├── bootstrap.rs     # App construction: shared LLM clients, tool-registry wiring, runtimes
        ├── image_loader.rs  # egui image loader for pasted/attached chat images
        ├── fonts.rs         # Custom font loading
        ├── logging.rs       # tracing setup + custom FormatEvent
        └── ui/              # chat, sessions, memory, mcp, usage, dashboard, improvements, agent config, ...
```

Module dependency rules are documented in [`wuffagent-core/src/lib.rs`](wuffagent-core/src/lib.rs): `types` is dependency-free, `client` depends on `usage` (one log line per completed LLM call), and the only 2-cycle is the thin `llm ↔ activity` pass-through.

## Prerequisites

- Rust toolchain (Rust 1.75+) with `cargo` on PATH — https://www.rust-lang.org/tools/install
- (Optional) Python 3 — only needed for the MCP stdio integration tests; tests skip themselves when no interpreter is available.

## Building & Running

```bash
cargo check                 # quick compile check
cargo test                  # run all tests
cargo build                 # build all crates (debug)
cargo build --release       # build for release

cargo run -p wuffagent-egui # run the GUI
```

Or run the built binary directly (Windows: `target\release\wuffagent-egui.exe`).

> **Windows note:** if the app is running, `cargo build` fails with
> `failed to remove file ...wuffagent-egui.exe (os error 5)` when relinking.
> Close WuffAgent before rebuilding.

## Configuration

All app data lives under the user home directory, `~/.wuffagent/` (created on first run; the config file is `~/.wuffagent/config.json`):

| File | Purpose |
|------|---------|
| `config.json` | Connection settings, model, presets, chat options, memory config, `mcp_servers`, `model_prices`, `metrics_retention_days` |
| `agents/<name>.json` | One agent profile per file (name, prompt, allowed tools, shell config, handoff/restart/hand_back/session-note/metrics flags) |
| `agents/history/` | Prompt-history snapshots of agent profiles (last 20 per agent; used by the revert feature) |
| `sessions/` | Persisted conversation sessions |
| `memories/<project>.json` | Per-project memory (default project: `default`) |
| `skills/` | Saved skills (procedural memory), one markdown file per skill |
| `evals/<agent>.jsonl` | Saved golden/regression evals, one JSON object per line |
| `metrics/<agent>.jsonl` | Per-agent run-metrics log (run/feedback/skill_use/trim/check/eval lines) |
| `metrics/rollups/` | Daily rollups of metrics older than the retention window |
| `usage.jsonl` | Append-only token-usage log (one JSON line per completed LLM call) |
| `restart.json` | One-shot marker written before a self-restart so the new process auto-resumes the session |

Native tool plugins are the one exception: they load from the platform config dir, `<platform config dir>/wuffagent/plugins` (e.g. `%APPDATA%\wuffagent\plugins` on Windows).

A sample agent profile (the repo ships `architect`, `coder`, `generalist` (in `general.json`), `researcher`, and the self-improving `wuffagent` meta-agent in [`agents/`](agents/)):

```json
{
  "name": "generalist",
  "description": "General purpose worker for tasks that don't fit other categories",
  "personality": "You are a versatile generalist worker...",
  "allowed_tools": ["read_file", "write_file", "append_file", "list_dir", "search_files", "search_content", "apply_diff", "mkdir", "delete", "copy", "move", "file_info", "calculation", "web_search", "fetch_url", "time"],
  "handoff_enabled": true,
  "handoff_targets": ["researcher", "coder"],
  "restart_enabled": true,
  "shell_config": {
    "shell_enabled": true,
    "allowed_commands": ["git (status|log|diff).*"],
    "shell_type": "powershell",
    "shell_timeout_ms": 60000
  }
}
```

Notes:
- `personality` is a legacy alias of `system_prompt`; `can_invoke` is a legacy alias of `handoff_targets`.
- `shell`, `handoff`, `restart`, `hand_back`, and `session_note` visibility is gated by their `*_enabled` flags (`handoff_enabled`, `restart_enabled`, `hand_back_enabled`, `session_note_enabled`), **not** by `allowed_tools`.
- `metrics_enabled` (default `true`) controls whether an agent's runs are written to the run-metrics store.
- An empty `allowed_tools` means the agent gets every registered tool (built-in, plugin, and MCP).

Profile discovery order: primary dir `~/.wuffagent/agents/`, then `./agents` (current working directory), then `<exe dir>/agents`. First-seen name wins; new/edited profiles are always written to the primary dir.

## Memory

Memory is a per-project JSON store (default `~/.wuffagent/memories/<project>.json`) managed entirely through the [`MemoryManager`](wuffagent-core/src/memory/manager.rs). There is no background batch extraction — agents are the only write path:

- **Tools** — `save_memory`, `update_memory`, `consolidate_memories`, and `delete_memory` let agents create, revise, merge, and prune entries. `save_memory` returns the new entry ID plus the top similar existing entries so the agent can update or consolidate instead of duplicating.
- **Dedup gate** — `MemoryManager::add` runs a keyword search first; strong matches return the existing entry (id + content) rather than inserting a near-duplicate.
- **Injection** — relevant memories are selected by the user's query (`search`) and injected into the agent system prompt; with no hits, Always mode falls back to the most recent active entries. `get_recent` skips superseded/expired entries.
- **Maintenance (opt-in)** — with `memory_maintenance` enabled, maintenance runs in small batches (`memory_maintenance_batch_size`), each bounded by `memory_maintenance_timeout_secs`. It auto-triggers after a task only when enabled, above `memory_maintenance_threshold`, and past the cooldown (1 step per 3 completed tasks), and can also be run manually from the memory panel.
- **Self-improvement** — when `auto_improve` is on, the engine calls `suggest_improvements` after a task (throttled by `improvement_cooldown_tasks` and only when new lesson/outcome/feedback evidence exists) and emits `ImprovementSuggested`; approving in the UI writes the updated agent JSON (or creates a new agent) and refreshes the registry. `run_self_improvement` triggers the same check on demand (bypassing the gates); `list_improvement_status` reports the loop's state.
- **UI** — the memory panel lists all entries, searches, edits content/tags, deletes (with confirm), and runs maintenance on demand; a memory settings section exposes enabled, injection mode, maintenance toggle/threshold, and the auto-improve toggle. The status bar shows an active-memory count with a tooltip.

## Skills (procedural memory)

Skills are reusable, named procedures that agents save and reuse across sessions. They live under `~/.wuffagent/skills/`, one markdown file per skill:

```markdown
---
name: git-rebase-workflow
description: How to rebase a feature branch in this repo
when_to_use: Before rebasing or force-pushing in a shared repo
---
Step-by-step body...
```

- **Storage** — the [`SkillStore`](wuffagent-core/src/memory/skills.rs) is read-through on every call (files are small and few); writes are atomic (temp file + rename) and corrupt files are skipped rather than fatal.
- **Injection** — each skill's frontmatter (name, description, when-to-use) is injected into agent system prompts (capped by `SkillStore::prompt_block`), so the model knows what procedures exist and can `read_skill` the full body when one looks relevant.
- **Tools** — `save_skill` (slug name, upserts by name), `list_skills` (name/description/when-to-use), `read_skill` (full body), and `delete_skill`. Successful `read_skill` calls are recorded as `skill_use` metrics lines.

## Run-metrics & cost

Beyond the per-LLM-call `usage.jsonl`, WuffAgent keeps a per-agent **run-metrics store** — one append-only JSONL file per agent at `~/.wuffagent/metrics/<agent>.jsonl` (see [`agents/metrics`](wuffagent-core/src/agents/metrics/mod.rs)). Each line is tagged with a `kind`:

- `run` — one per agent task run: tool calls, tool errors, verification attempts, wall-clock duration, terminal `outcome` (`verified` / `verified_after_retry` / `gave_up` / `none`), token totals, and estimated `cost_usd`.
- `feedback` — user thumbs up/down, either per-run (`target: run:<run_id>`) or per-message.
- `skill_use` — one per successful `read_skill`.
- `trim` — one per context-trimming decision (kind + freed estimate).
- `check` — one per self-improvement check (agent, produced, and the *previous* check's effect verdict).
- `eval` — one per headless golden-evaluation run (eval id, pass/fail, cost).

Writes are best-effort (fire-and-forget; a failure never breaks the agent), and the reader skips blank/corrupt lines (forward-compatible: older binaries ignore new kinds).

- **`read_metrics` tool** — agent-readable view: windowed aggregates + recent raw lines for one agent, or a fleet-wide one-line-per-agent overview; `status=true` reports the improvement-loop state, and `compare=true` gives a before/after window.
- **Fleet dashboard** — a floating UI panel (per-agent KPI cards, a 30-day trend chart, and an improvement-loop state table) that shares the exact same `fleet_loop_status` core the `read_metrics status=true` tool renders as text.
- **Cost** — each run/eval line carries an estimated USD cost driven by the `model_prices` table in `config.json` (USD per 1M tokens, prompt and completion billed separately). An empty table or unknown model means "recorded but unpriced" (cost 0.0); matching is case-insensitive exact (`usage::cost_usd`).
- **Retention & rollup** — at startup (once per calendar day), fully-elapsed days older than `metrics_retention_days` (default 90) are rewritten to per-day rollup files under `metrics/rollups/` and the raw run lines are pruned, keeping the hot file small and disk bounded.

## Evals (golden/regression)

Evals are saved per-agent tasks with verification criteria, stored one-per-line under `~/.wuffagent/evals/<agent>.jsonl` (`{id, task, expect, max_tool_calls?}`):

- **Tools** — `save_eval` (upsert by `id`), `list_evals`, `delete_eval`.
- **`run_eval`** — runs a profile's evals **headlessly**, each on a fresh, isolated agent (empty conversation store, a per-agent `ToolManager` view, `metrics_enabled = false` so it doesn't pollute real-run metrics), bounded by a per-eval wall-clock timeout. The existing verification judge grades each final response against the eval's `expect`; the pass/fail + cost is written as an `Eval` metrics line. This is the before/after regression signal after prompt/profile changes.

## Web search

`web_search` (`{query, max_results?}` → `{query, results: [{title, url, snippet}]}`) and `fetch_url` (`{url, max_bytes?}` → page content as plain text) let agents research the web.

- **Backends** — `Auto` (default: Bing → Yahoo → DuckDuckGo, automatic failover on error/empty results), `Bing`, `Yahoo`, `DuckDuckGo`, `SearXNG` (self-hosted, needs a base URL). `Brave` (API key) also exists as a compat-only backend but is never created by the UI. Configured in the `search_config` block of `config.json` (including a region code) or in Settings → **Web search**; backend changes take effect on restart.
- **Env overrides** (headless use; take precedence over config) — `WUFFAGENT_SEARCH_BACKEND=auto|bing|yahoo|duckduckgo|searxng` (case-insensitive; selecting `searxng` without any URL keeps the configured backend) and `WUFFAGENT_SEARXNG_URL=https://…` (overrides the SearXNG base URL whenever the resolved backend is SearXNG).
- **Caching** — results are cached in memory with a TTL (`search_config.cache_duration_secs`, default 300 s), so repeated queries within the window cost no HTTP requests.

## MCP (Model Context Protocol)

WuffAgent acts as an MCP **client**: it connects to external MCP servers, discovers their tools, and bridges them into the shared tool registry so agents can call them like built-ins.

- **Transports** — `Stdio` (spawn a child process and speak line-delimited JSON-RPC over its stdin/stdout) and `Http` (MCP Streamable HTTP endpoint, with optional headers).
- **Protocol** — MCP revision 2025-06-18, hand-rolled JSON-RPC 2.0 in `wuffagent-core/src/tools/mcp/` (no external SDK).
- **Tool names** — each server tool is registered as `mcp__<server>__<tool>`. An agent with an empty `allowed_tools` sees them automatically; otherwise list the full `mcp__...` name. Per-server `allowed_tools` is an allowlist of which server tools to expose at all (empty = all).
- **Runtime** — all MCP I/O runs on a dedicated 2-worker tokio runtime owned by `McpManager` (the UI thread lives in the main runtime, where `block_on` is not allowed).
- **Lifecycle** — servers with `enabled: true` auto-connect at startup (failures are logged and retryable from the panel); the MCP panel supports add/edit/delete, connect/disconnect, per-server enable/disable, per-tool enable/disable, and per-call timeouts (`timeout_secs`, default 60). The `mcp_*` tools (`mcp_list`, `mcp_add_server`, `mcp_connect`, `mcp_disconnect`, `mcp_remove_server`, `mcp_refresh_tools`, `mcp_set_tool_enabled`) do the same from within an agent.

Servers are configured in the `mcp_servers` array of `config.json` (old configs without the key load with an empty list):

```json
{
  "mcp_servers": [
    {
      "name": "fs",
      "transport": {
        "Stdio": {
          "command": "npx",
          "args": ["-y", "@modelcontextprotocol/server-filesystem", "C:\\data"],
          "env": {},
          "working_dir": null
        }
      },
      "enabled": true,
      "timeout_secs": 60,
      "allowed_tools": []
    },
    {
      "name": "remote",
      "transport": {
        "Http": {
          "url": "https://example.com/mcp",
          "headers": { "Authorization": "Bearer <token>" }
        }
      }
    }
  ]
}
```

Server names are restricted to `[a-zA-Z0-9_-]` because they become part of the generated tool names.

## Token usage

Every completed LLM call appends one JSON line to `~/.wuffagent/usage.jsonl`:

```json
{"ts":"2026-09-21T12:34:56.789Z","session_id":"…","agent":"general","model":"deepseek-chat","prompt_tokens":12345,"completion_tokens":678,"total_tokens":13023}
```

Writes are best-effort — a log failure is reported via `tracing` and never breaks the chat. The **usage panel** (📈) charts input/output/total tokens per bucket over the last 24 hours, 30 days, or 12 weeks. The log is read incrementally (new lines appended after each stream completion; full rescan only if the file shrank), and bucketing is done on local wall-clock time so DST transitions can't shift bucket boundaries.

## Self-restart

The per-agent `restart` tool (enabled by default, gated by `restart_enabled`) lets an agent rebuild and relaunch WuffAgent to pick up freshly built code, then continue the same session:

1. The tool optionally runs `build_cmd` in the shell (600 s timeout; output captured to a temp log whose tail is returned on failure).
2. WuffAgent saves the session, writes a one-shot `restart.json` marker (session id + reason), relaunches the binary, and closes the window.
3. The new process reads and deletes the marker at startup, makes that session active, and posts a continue turn containing the reason so the agent picks the work back up.

On Windows you cannot relink the running exe, so for WuffAgent itself use a separate target dir, e.g. `build_cmd="cargo build --target-dir target/relaunch"` with the new binary at `target\relaunch\debug\wuffagent-egui.exe`.

## Development Notes

- See [AGENTS.md](AGENTS.md) for agent-oriented guidance and [BUILD.md](BUILD.md) for build details.
- Tests are mostly inline `#[cfg(test)]` modules (some split into sibling `tests.rs` files); run a single test with `cargo test -p wuffagent-core <test_name>`.
- New persisted JSON files should use the atomic temp-file + rename pattern (see `wuffagent-core/src/memory/storage.rs`).
