# T2: MCP management tools

**Status:** implemented
**Part of:** `plans/self-improvement-gaps.md` Phase 3 (first-class self-modification), item T2.

## Goal

Give agents first-class tools to manage the MCP subsystem (same capabilities the MCP panel has):

- `mcp_list` — every configured server with live status (`configured` / `connecting` /
  `connected {tool_count}` / `error: …`), transport summary, timeout, and the full tool
  list (registry names `mcp__<server>__<tool>`, per-tool enabled flag, description).
- `mcp_add_server` — add or replace a server entry:
  - stdio (default): `command`, `args`, `env`, `working_dir`
  - http: `transport: "http"`, `url`, `headers`
  - plus `enabled` (default true), `timeout_secs` (default 60), `allowed_tools` (empty = all)
  - `connect_now` (default = `enabled`): connect immediately and report the registered tool count
- `mcp_connect` / `mcp_disconnect` — connect (handshake + `tools/list` + register) /
  disconnect (unregister tools, kill process).
- `mcp_remove_server` — remove from live state AND from `config.json`.
- `mcp_refresh_tools` — re-run `tools/list` on a connected server (tool set may have changed).
- `mcp_set_tool_enabled` — enable/disable one server tool (register/unregister in the
  shared registry).

## Design decisions

1. **Shared-registry tools** (like T1's profile tools), NOT per-execution: they only need
   the app's `McpManager`, which is static for the process lifetime. Registered once in
   `wuffagent-egui/src/main.rs` via `builtin::register_mcp_tools(registry, Arc<McpManager>, Option<Arc<Mutex<Sender<AppEvent>>>>, Option<String>)`,
   right after the MCP manager is created (before `AgentEngine`).
2. **Gated by `allowed_tools`** like every other tool (empty list = all tools). An agent
   that wants MCP management lists the `mcp_*` names; agents that only *use* MCP tools
   list the `mcp__<server>__<tool>` names.
3. **No new runtime needed.** `McpManager`'s `*_sync` facades `block_on` its OWN dedicated
   runtime — but a tool executes on `tokio::task::spawn_blocking` (inside the MAIN
   runtime's context), where `block_on` is not allowed. So every blocking op runs on a
   fresh **plain std thread** (`run_mcp_op`), with a `recv_timeout` wall-clock ceiling
   (30 s connect/refresh, 10 s disconnect/remove) mirroring the UI panel's per-op
   timeouts. On timeout the op's thread is abandoned (it finishes on its own) and the
   tool reports the timeout.
4. **Config persistence mirrors the UI panel** (`mcp_panel::apply_edit`): `upsert_server`
   (live state) → read-modify-write of `config.json`'s `mcp_servers` array → save. The
   read-modify-write is **atomic** (temp file + rename, the same pattern as memory
   storage) and preserves every other config field (the file is parsed as the full
   `Config` before rewriting). A config-file write failure does NOT fail the tool: the
   change is live for this run, and the result carries a `warning` saying it will not
   survive a restart.
5. **Cross-thread config writes** are serialized with a process-wide `static` lock
   (add/remove tools share it), so two tool calls can't clobber each other's
   read-modify-write. (The UI panel is single-threaded and needs no such lock.)
6. **Test-only config path override**: `config::set_config_path_for_testing(Option<PathBuf>)`
   (a `OnceLock<Mutex<Option<PathBuf>>>` in `config/paths.rs`) lets tests point
   `get_config_path()` at a temp file — the tools persist through it, so no test ever
   touches the real `~/.wuffagent/config.json`.
7. **`AppEvent::McpConfigChanged`** (added in T3a): `mcp_add_server` and
   `mcp_remove_server` emit this event after persisting to `config.json`; the UI
   handler reloads `config.mcp_servers` from disk so the in-memory copy (used by
   the panel's edit dialog) stays in sync without an app restart. The event sender
   is wired at registration time (`register_mcp_tools` takes an
   `Option<Arc<Mutex<Sender<AppEvent>>>>` + `session_id`).

## Changes

| File | Change |
|---|---|
| `wuffagent-core/src/tools/builtin/mcp.rs` (new) | the 7 tools + `parse_server_config` + `update_mcp_servers_in_config` (atomic RMW) + `run_mcp_op` (plain-thread op with timeout) + config-write lock |
| `wuffagent-core/src/tools/builtin/mcp/tests.rs` (new) | parse defaults/errors (stdio+http), validation, RMW add/remove roundtrip, atomic write preserves unrelated fields, `run_mcp_op` ok/err |
| `wuffagent-core/src/tools/builtin/mod.rs` | `pub mod mcp` + re-exports + `register_mcp_tools` |
| `wuffagent-core/src/config/paths.rs` | `set_config_path_for_testing` override (OnceLock) |
| `wuffagent-core/src/config/mod.rs` | re-export the test override |
| `wuffagent-egui/src/main.rs` | `register_mcp_tools(&registry, mcp_manager.clone(), Some(Arc::new(Mutex::new(event_tx.clone()))), None)` after the MCP manager is built |

## Verified

- `cargo build -p wuffagent-core` / `-p wuffagent-egui` clean; `cargo test -p wuffagent-core`
  green (507 + 1 doc, incl. 7 new mcp tool tests); `cargo test -p wuffagent-egui` green (24).
- End-to-end connect/disconnect paths are already covered by the manager integration tests
  (`tools/mcp/mod.rs`, python mock server).

## Dogfood

The running `wuffagent` profile gains the `mcp_*` tools on the next message (policy is
resolved at send time); first real use: `mcp_list` should show the current server set.

## Out of scope

- Per-tool `allowed_tools` editing beyond `mcp_set_tool_enabled` (the config-level
  allowlist IS editable via `mcp_add_server`).
- An `AppEvent::McpChanged` (see decision 7).
- T3 (runtime plugin reload) and T4 (safer restart) — separate items.
