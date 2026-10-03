# Telegram bot plugin — talk to WuffAgent from Telegram

**Date:** 2026-07-07
**Scope:** new `plugins/telegram_plugin` crate (cdylib) + a small, backward-compatible
extension of the plugin ABI (an *optional* host-API callback table) + a thin egui
bridge. No new executable — the bot runs as a thread inside the running WuffAgent
process, per the requirement "a plugin, not an extra exe".

## Requirements

- User messages from Telegram (private chat or group) reach a WuffAgent session and
  run a full agent turn (LLM + tools + MCP tools + plugins — the app's real registry).
- The agent's reply is sent back to Telegram.
- The plugin is installed like `hello_plugin`: build → copy `.dll` to
  `~/.wuffagent/plugins/` → `reload_plugins` (or app restart). Auto-starts if a config
  file is present.
- Existing plugins keep loading (ABI backward compatible).

## Why the current plugin ABI is not enough (verified)

The plugin ABI (`wuffagent-core/src/tools/types` + `plugins/hello_plugin/src/lib.rs`)
exports exactly three symbols: `wuff_tool_abi_version`, `wuff_tool_metadata`,
`wuff_tool_create`. A plugin can only *expose a Tool the agent calls*
(`execute(ToolParams) -> ToolResult`). It cannot:

1. **inject a user message** into a live session — the session store
   (`HashMap<String, SessionRuntime>`) and the pipeline driver
   (`start_pipeline_for_session`, `wuffagent-egui/src/ui/input/mod.rs:482`) live in
   the egui app; nothing hands the plugin a handle to them.
2. **receive pipeline events** — `AppEvent` (`wuffagent-core/src/types/events.rs`)
   flows core→UI over the app's own mpsc; there is no plugin-facing channel.

So a pure tool plugin could at best *send* to Telegram, not *talk to* the running
agent. The plan therefore adds a minimal, optional host-API vtable.

## Architecture

```
Telegram ⇄ api.telegram.org (HTTPS long-poll / sendMessage)
                │
   telegram_plugin (cdylib, loaded via reload_plugins)
   ┌───────────────────────────────────────────────┐
   │ poller thread (dedicated OS thread, blocking  │
   │ reqwest): getUpdates → allowlist → HostApi::  │
   │ inject_user_message; HostApi event callback   │
   │ accumulates StreamChunk, sends final text on  │
   │ StreamComplete / StreamError                  │
   │ `telegram` tool: start | stop | status | send │
   └──────────────┬────────────────────────────────┘
                  │ HostApi vtable (C fn-ptrs, passed at load)
   WuffAgent app (egui main thread)
   ├─ drains inject queue → start_pipeline_for_session(bot_session, text)
   └─ forwards AppEvents (filtered to bot session) → plugin callback
        → real pipeline: LLM (llama server / API), full tool registry incl. MCP
```

Key properties:

- **Dedicated bot session** (MVP): the bot talks in its own session (default name
  `Telegram`, configurable), so it never interleaves with the user's interactive
  chat. The session is a normal session file under `~/.wuffagent/sessions/` — the
  GUI can open it, and its runs show up in metrics / fleet dashboard / memory
  exactly like any other run (they go through the real pipeline).
- **Full tool access**: because the turn runs in the *app's* pipeline, the bot's
  agent has the app's registry — builtin tools, MCP server tools, other plugins.
  (A headless in-plugin loop would have seen only builtin+plugin tools; that is the
  reason for the host API.)
- **Plugin-process reality (dual linking)**: the cdylib statically links its own
  copy of `wuffagent-core` and its deps (same as `hello_plugin`). Consequences:
  - the vtable pointer is the *only* thing handed across; types are layout-
    compatible because both copies compile from the same core source;
  - `tracing!` inside the plugin goes to the plugin's own (unset) global
    dispatcher → **silently dropped**. The plugin logs via `eprintln!` (app
    console) + a small append-only log at `~/.wuffagent/telegram.log`.
  - plugins are never unloaded (live until process exit) → host-side callback
    slots also live until process exit; no teardown needed, `stop` just halts
    the poller.

## ABI extension (backward compatible)

New **optional** 4th export, checked by the loader like `get`-or-missing:

```
wuff_tool_host_api(host_api: *const HostApi) -> ()   // may be NULL
```

```rust
/// C-compatible fn-pointer table. Lives in `wuffagent-core::tools::types`
/// (next to ToolMetadata / PluginTool). `#[repr(C)]`, no Rust types by value.
pub struct HostApi {
    pub version: u32,                       // HOST_API_VERSION; plugin must check
    /// Enqueue a user message for `session_id` (bytes, UTF-8). Returns false if
    /// the app is shutting down. Implemented by the egui app (drains into
    /// start_pipeline_for_session on the UI thread).
    pub inject_user_message:
        extern "C" fn(session_id: *const u8, session_len: usize,
                      text: *const u8, text_len: usize) -> bool,
    /// Register a callback, invoked on the UI thread for each AppEvent whose
    /// session_id equals the filter (bytes). payload is a host-owned C string
    /// valid for the call only (kind 0: chunk text; 1: full final content;
    /// 2: error message; 3: round-complete marker, payload empty).
    /// MUST be fast + non-blocking (the callback runs while the UI frame is
    /// being drawn) — implementation just forwards into a std mpsc.
    pub register_event_callback:
        extern "C" fn(session_filter: *const u8, filter_len: usize,
                      cb: extern "C" fn(kind: u32, payload: *const u8,
                                        payload_len: usize,
                                        user_data: *mut core::ffi::c_void),
                      user_data: *mut core::ffi::c_void),
}
```

Loader changes (`wuffagent-core/src/tools/registry.rs`):

- `ToolRegistry::set_host_api(api: Option<std::ptr::NonNull<HostApi>>)` — the egui
  app sets it once at bootstrap, *before* the first plugin load; the registry
  stores it.
- After `wuff_tool_create` succeeds, the loader looks up
  `wuff_tool_host_api` in the loaded `Library`; if present, calls it with the
  stored pointer (or null). Missing symbol → no call (old plugins unaffected).
- **No `PLUGIN_ABI_VERSION` bump** (existing three exports unchanged; the new
  symbol is opt-in). `HOST_API_VERSION: u32 = 1` is separate; the plugin
  degrades to tool-only mode if the version is unrecognized.

## Plugin design (`plugins/telegram_plugin`)

Crate: cdylib, deps: `wuffagent-core` (path), `reqwest` (blocking feature, or a
1-worker tokio runtime — the `McpManager` owns-a-2-worker-runtime is the
in-process precedent), `serde`/`serde_json` for config + Telegram JSON.

**Config** — `~/.wuffagent/telegram.json` (plugin-owned file; sample in README):

```json
{
  "token": "123456:ABC...",
  "allow_chat_ids": [123456789],
  "session": "Telegram",          // session name (or id) the bot talks in
  "agent_profile": null,          // null = session's current agent selection
  "chunk_chars": 4096
}
```

- `allow_chat_ids` is mandatory (a bot token is full access to the bot; the
  allowlist is the security boundary). Unknown chat id → ignored (optionally a
  one-time "not authorized" reply, config flag).
- Token stays in the local config file (same trust level as the LLM API key in
  the app config). Note Windows ACL hardening as a doc remark, not code.

**Poller** (dedicated OS thread, started via `std::sync::Once`):

1. `getUpdates?offset=<n+1>&timeout=50` long-poll (HTTP timeout 65 s > 50 s
   server hold). `offset` persists in memory + last-seen `update_id` for dedupe.
2. On `message`: filter by allowlist; strip nothing (plain text only in MVP).
3. Send `sendChatAction` (`typing`) before injecting; clear the accumulated reply
   buffer; `HostApi::inject_user_message(session_id, text)`.
4. Event callback (UI thread): kind 0 → append to buffer; kind 1 → buffer
   replaced by full content, mark final; kind 2 → error text, mark final; kind 3
   → round boundary (MVP: ignore).
5. On final: chunk at ≤4096 chars (split on newline, then hard split — Telegram
   hard limit), `sendMessage` per chunk **plain text** (no parse_mode → no
   MarkdownV2 escaping failures in MVP).
6. Serialization: one in-flight turn per chat id (std Mutex<Option<...>> queue);
   a second message while busy is queued (MVP: max 1 queued, else reply "still
   working — try again in a moment").
7. Failure handling: 409 `Conflict` (another poller — e.g. two WuffAgent builds
   running) → back off 30 s, log + `telegram status` shows the conflict; do not
   crash the loop. 401 → "bad token, bot stopped". LLM-side errors arrive as
   kind-2 events → forwarded to the chat.
8. Log to `~/.wuffagent/telegram.log` (append, small rotation cap) + `eprintln!`.

**`telegram` tool** (the standard 3-symbol exports, name `telegram`):

| action | params | result |
|---|---|---|
| `start` | — | reads config, starts poller (idempotent) |
| `stop` | — | stops poller (config/token changes) |
| `status` | — | running? poller state, last update id, last error, 409 flag |
| `send` | `to` (chat id), `text` | proactive bot→user message (lets the *agent* message the user, e.g. "done" notifications) |

Auto-start: if the config file parses at `wuff_tool_create` time, start the
poller immediately (so "install → reload_plugins → talk" works with no further
interaction). `wuff_tool_host_api` stores the vtable in a `static AtomicPtr`
before the poller can use it (the loader calls it at load time; if it arrives as
null — e.g. loaded by a non-app host — the tool reports `host_api: missing` and
`start` degrades to send-only mode with a clear error on inject).

**Session bootstrap:** the egui bridge's `inject_user_message` must tolerate a
bot session that does not exist yet → create it (name from config) before
starting the pipeline (the sessions panel will then show it like any session).

## Phases

### P0 — core: HostApi + loader plumbing
- [ ] `HostApi` struct + `HOST_API_VERSION` in `tools/types` (`#[repr(C)]`).
- [ ] `ToolRegistry::set_host_api(Option<NonNull<HostApi>>)`.
- [ ] Loader: after `wuff_tool_create`, `lib.get(b"wuff_tool_host_api")` → call
      with stored ptr/null; `PluginLoadOutcome` unchanged.
- [ ] Unit tests: registry without host_api set + fake plugin that exports the
      symbol (build a tiny in-tree cdylib? — if too heavy for a unit test, cover
      via the loader's get-missing path + the P2 integration test); version
      check in the plugin-side helper.
- [ ] `cargo test -p wuffagent-core` green; `hello_plugin` still loads (manual:
      `reload_plugins` → `hello`).

### P1 — egui: host bridge
- [ ] Implement the two vtable fns (extern "C", no panics across the boundary —
      wrap bodies in `std::panic::catch_unwind`, log on panic):
  - `inject_user_message` → `std::sync::mpsc` send `(session_id, text)`; the
    egui update loop drains it each frame (same housekeeping slot as
    `process_pending_events`) → ensure session exists →
    `start_pipeline_for_session(...)` with the session's current agent.
  - `register_event_callback` → store `(filter, cb, user_data)` in a small
    app-side slot (single slot is enough: one bot; document it). UI-thread
    event processing: before/while handling `AppEvent`s for the bot session,
    call the cb with the right kind/payload (cheap C-string copies).
- [ ] Register the vtable at bootstrap via `registry.set_host_api(...)` before
    any plugin load; `request_repaint` where needed (typing indicator not needed
    — it's a Telegram-side action).
- [ ] `cargo check -p wuffagent-egui` + egui test suite green.

### P2 — telegram_plugin crate
- [ ] Crate skeleton (cdylib, `Cargo.toml` mirroring `hello_plugin`), exports the
      4 symbols, `telegram` tool with the table above.
- [ ] Config load/validate (`telegram.json`), log file, `Once`-guarded poller
      thread, blocking reqwest (client built **once** — the llama-integration
      P1 lesson), offset/dedupe state.
- [ ] Telegram calls: `getUpdates` (long-poll), `sendMessage` (plain, chunked),
      `sendChatAction`. 401/409/5xx handling per design.
- [ ] Reply accumulation via the registered event callback (std mpsc relay from
      UI thread → poller thread).
- [ ] `README.md`: setup (BotFather token, allowlist, build, install,
      `reload_plugins`), config reference, troubleshooting (409, bad token,
      server down), the dual-linking logging note.

### P3 — verification, docs, close-out
- [ ] Unit tests (plugin crate): chunking (boundary 4096/4100, CRLF), allowlist
      filter, offset dedupe, config parse errors, version-check degrade path.
- [ ] Integration test with a **fake Telegram server** (in-test `TcpListener`
      serving canned `getUpdates`/`sendMessage` JSON; point the client at
      `http://127.0.0.1:<port>` via a config override field `api_base` — add it
      in P2) asserting the full message→inject→callback→sendMessage round trip.
- [ ] Manual E2E (needs a real bot token — **ask the user to run this**):
      install plugin, `reload_plugins`, message the bot, verify the reply in
      Telegram AND the turn visible in the GUI's `Telegram` session + fleet
      dashboard metrics.
- [ ] Main README: short "Plugins → Telegram bot" section; `git commit` plan +
      code; save a memory entry (agent:wuffagent) with the ABI-extension shape.

## Risks / mitigations

| Risk | Mitigation |
|---|---|
| Callback invoked mid-frame does slow work → UI jank | cb contract: forward to mpsc only; host-side copy is a small C-string; review in P1 |
| Two pollers (two app builds / dev + release) → 409 storm | 30 s backoff + `status` flag; documented |
| Token leaked via `status` output | `status` prints only token prefix `1234…` |
| Bot session grows unbounded (context) | It's a normal session — the existing trimming/context-overflow machinery applies unchanged |
| Plugin built against older core (vtable layout drift) | Separate `HOST_API_VERSION` + plugin degrades to send-only with a clear error; same stale-DLL philosophy as `PLUGIN_ABI_VERSION` |
| `extern "C"` boundary panic aborts the app | `catch_unwind` in both host fns; plugin cb never allocates across the boundary beyond the mpsc send |

## Explicitly out of scope (follow-ups)

- `session: "active"` mode (bot joins the user's currently open session).
- Rich formatting (MarkdownV2 with escaping), images/voice in/out, group topics.
- Plugin *unload* (ABI-wide, not bot-specific) — `stop` covers the practical need.
- Multi-bot configs (one token per plugin instance).
