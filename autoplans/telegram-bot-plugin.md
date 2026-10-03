# Telegram bot plugin — talk to WuffAgent from Telegram

**Date:** 2026-07-07 (rev 2: + session create/join/switch)
**Scope:** new `plugins/telegram_plugin` crate (cdylib) + a small, backward-compatible
extension of the plugin ABI (an *optional* host-API callback table) + a thin egui
bridge. No new executable — the bot runs as a thread inside the running WuffAgent
process, per the requirement "a plugin, not an extra exe".

## Requirements

- User messages from Telegram (private chat or group) reach a WuffAgent session and
  run a full agent turn (LLM + tools + MCP tools + plugins — the app's real registry).
- **Session control from Telegram:** the bot can **create** sessions, **join** any
  existing one (incl. sessions created in the GUI), and **switch** between them —
  independently per Telegram chat.
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
2. **manage sessions** — create/list/resolve are egui-side
   (`apply_sessions_action`, the `session_store` in `ChatApp`).
3. **receive pipeline events** — `AppEvent` (`wuffagent-core/src/types/events.rs`)
   flows core→UI over the app's own mpsc; there is no plugin-facing channel.

So a pure tool plugin could at best *send* to Telegram, not *talk to* the running
agent. The plan therefore adds a minimal, optional host-API vtable.

Session model facts (verified): `SessionRuntime` carries `session_id: String` and
`name: String` (display name, renamable in the GUI; sessions panel renders
`session.name` + count + preview + timestamp). Ids look like
`session_{millis}_{8hex}` (~30 chars). GUI create/rename already exist — the bridge
reuses them rather than duplicating session logic.

## Architecture

```
Telegram ⇄ api.telegram.org (HTTPS long-poll / sendMessage)
                │
   telegram_plugin (cdylib, loaded via reload_plugins)
   ┌───────────────────────────────────────────────┐
   │ poller thread (dedicated OS thread, blocking  │
   │ reqwest): getUpdates → allowlist → slash-     │
   │ commands (/new /sessions /use /current /help) │
   │ or free text → per-chat current session →     │
   │ HostApi::inject_user_message(session_id, text)│
   │ Event callback (all sessions, sid in payload) │
   │ filters to each chat's current session,       │
   │ accumulates StreamChunk, sends final text on  │
   │ StreamComplete / StreamError                  │
   │ `telegram` tool: start|stop|status|send +     │
   │ new_session|list_sessions|use_session         │
   └──────────────┬────────────────────────────────┘
                  │ HostApi vtable (C fn-ptrs, passed at load)
   WuffAgent app (egui main thread)
   ├─ drains command queue → create_session / list / resolve (reply via
   │   std-mpsc oneshot) / inject → start_pipeline_for_session(sid, text)
   └─ forwards AppEvents (chunk/round/complete/error + session_id) →
        plugin callback
        → real pipeline: LLM (llama server / API), full tool registry incl. MCP
```

Key properties:

- **Per-chat sessions with full control (core requirement):** each Telegram chat id
  has its own "current session" (plugin state, persisted in
  `~/.wuffagent/telegram-state.json`). Default on first use: a `Telegram` session
  (auto-created on first free-text message, so nothing needs pre-setup). Slash
  commands then allow `/new [name]` (create + switch), `/sessions` (list all,
  `*` marks current), `/use <name|id>` (join ANY session — including ones created
  or renamed in the GUI), `/current`, `/help`. GUI and bot see the same session
  store: a `/new`-ed session appears in the GUI sessions panel immediately, and a
  GUI session can be joined with `/use`.
- **Full tool access:** because the turn runs in the *app's* pipeline, the bot's
  agent has the app's registry — builtin tools, MCP server tools, other plugins.
  (A headless in-plugin loop would have seen only builtin+plugin tools; that is
  the reason for the host API.)
- **Shared-session semantics (documented, not solved):** joining a session the user
  is also driving in the GUI means both sides append to the same conversation;
  the existing abort-on-new-send pipeline semantics apply (a new send replaces the
  in-flight run). No per-side locking in MVP.
- **Plugin-process reality (dual linking):** the cdylib statically links its own
  copy of `wuffagent-core` and its deps (same as `hello_plugin`). Consequences:
  - the vtable pointer is the *only* thing handed across; types are layout-
    compatible because both copies compile from the same core source;
  - `tracing!` inside the plugin goes to the plugin's own (unset) global
    dispatcher → **silently dropped**. The plugin logs via `eprintln!` (app
    console) + a small append-only log at `~/.wuffagent/telegram.log`.
  - plugins are never unloaded (live until process exit) → host-side state
    (command queue, callback slot) also lives until process exit; `stop` just
    halts the poller.

## ABI extension (backward compatible)

New **optional** 4th export, checked by the loader like `get`-or-missing:

```
wuff_tool_host_api(host_api: *const HostApi) -> ()   // may be NULL
```

```rust
/// C-compatible fn-pointer table. Lives in `wuffagent-core::tools::types`
/// (next to ToolMetadata / PluginTool). `#[repr(C)]`, no Rust types by value.
///
/// Blocking contract: `inject_user_message` / `create_session` /
/// `resolve_session` / `session_count` / `get_session` are command+wait
/// (≤5 s timeout) — safe from the plugin's poller thread and from tool
/// execution, but MUST NOT be called from the UI thread (deadlock).
pub struct HostApi {
    pub version: u32,                       // HOST_API_VERSION; plugin must check
    /// Enqueue a user message for `session_id` (bytes, UTF-8). Auto-creates the
    /// session (name = id tail) if missing. Returns false if the app is busy
    /// beyond the timeout or shutting down.
    pub inject_user_message:
        extern "C" fn(session_id: *const u8, session_len: usize,
                      text: *const u8, text_len: usize) -> bool,
    /// Create a named session; writes the new id (NUL-terminated) into
    /// out_id/out_cap. Returns false on failure or buffer too small (cap < 64).
    pub create_session:
        extern "C" fn(name: *const u8, name_len: usize,
                      out_id: *mut u8, out_cap: usize) -> bool,
    /// Resolve a session by exact id, or by name (case-insensitive).
    pub resolve_session:
        extern "C" fn(query: *const u8, query_len: usize,
                      out_id: *mut u8, out_cap: usize) -> bool,
    /// Number of sessions in the store.
    pub session_count: extern "C" fn() -> usize,
    /// Write session `i`'s id and name into the two out buffers (both
    /// NUL-terminated, independent caps). Returns false if i out of range.
    pub get_session:
        extern "C" fn(index: usize,
                      out_id: *mut u8, id_cap: usize,
                      out_name: *mut u8, name_cap: usize) -> bool,
    /// Register the event callback (single slot; re-registration replaces).
    /// Invoked on the UI thread for pipeline AppEvents of ANY session —
    /// the plugin filters by session_id itself (session switches are
    /// plugin-side state; a host-side filter slot would go stale on /use).
    /// payload per kind: 0 = chunk text; 1 = full final content;
    /// 2 = error message; 3 = round-complete marker (payload empty).
    /// Host-owned C strings, valid for the call only.
    /// MUST be fast + non-blocking (runs while the UI frame is drawn) —
    /// implementation just forwards into a std mpsc.
    pub register_event_callback:
        extern "C" fn(cb: extern "C" fn(kind: u32,
                                        session_id: *const u8, sid_len: usize,
                                        payload: *const u8, payload_len: usize,
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
  degrades to send-only mode if the version is unrecognized.

## Plugin design (`plugins/telegram_plugin`)

Crate: cdylib, deps: `wuffagent-core` (path), `reqwest` (blocking feature, or a
1-worker tokio runtime — the `McpManager` owns-a-2-worker-runtime is the
in-process precedent), `serde`/`serde_json` for config + Telegram JSON.

**Config** — `~/.wuffagent/telegram.json` (plugin-owned file; sample in README):

```json
{
  "token": "123456:ABC...",
  "allow_chat_ids": [123456789],
  "default_session": "Telegram",  // session each chat starts in (auto-created)
  "chunk_chars": 4096,
  "api_base": null                // test override: full base URL of the Bot API
}
```

- `allow_chat_ids` is mandatory (a bot token is full access to the bot; the
  allowlist is the security boundary). Unknown chat id → ignored (optionally a
  one-time "not authorized" reply, config flag).
- Token stays in the local config file (same trust level as the LLM API key in
  the app config). Note Windows ACL hardening as a doc remark, not code.

**Per-chat state** — `~/.wuffagent/telegram-state.json`:
`{ "<chat_id>": { "session_id": "...", "session_name": "..." } }`, written on
every switch/create (atomic temp+rename). Loaded at poller start. This is what
makes each Telegram chat keep its own conversation context across app restarts.

**Poller** (dedicated OS thread, started via `std::sync::Once`):

1. `getUpdates?offset=<n+1>&timeout=50` long-poll (HTTP timeout 65 s > 50 s
   server hold). `offset` persists in memory + last-seen `update_id` for dedupe.
2. On `message`: filter by allowlist; then dispatch:
   - **Slash commands** (case-insensitive, plain-text prefixes — no Bot API
     command menu needed in MVP):
     - `/new [name]` → `create_session` (default name `Telegram <HH:mm>`);
       switch the chat to it; reply `✔ session '<name>' created`.
     - `/sessions` → `session_count` + `get_session` loop → numbered list,
       `*` on the chat's current one (names truncated to 40 chars).
     - `/use <name|id>` → `resolve_session`; on success switch + reply
       `→ now in '<name>'`; on failure reply with the current `/sessions` list.
     - `/current` → `● <name> (<id-tail>)`.
     - `/help` → command list.
     - Unknown `/…` → short "unknown command, see /help".
   - **Free text** → current session for this chat (first-ever message:
     `create_session(default_session)` once). Send `sendChatAction(typing)`,
     reset the reply buffer, `inject_user_message(session_id, text)`.
3. Event callback (UI thread → relay mpsc): match `session_id` against the
   chat's current session (a turn always belongs to the session it was sent in —
   the poller records `inflight: (chat_id, session_id)` at send time and only
   accepts events for that pair, so a `/use` mid-turn doesn't misroute the old
   turn's tail); kind 0 → append; kind 1 → replace + final; kind 2 → error +
   final; kind 3 → ignore (MVP).
4. On final: chunk at ≤4096 chars (split on newline, then hard split),
   `sendMessage` per chunk **plain text** (no parse_mode in MVP).
5. Serialization: one in-flight turn per chat id; a second free-text message
   while busy is queued (max 1; else "still working — try again in a moment").
   Slash commands are always processed (they don't block on the LLM).
6. Failure handling: 409 `Conflict` (two pollers — e.g. two WuffAgent builds)
   → 30 s backoff, log + visible in `status`; 401 → "bad token, bot stopped";
   inject/resolve failure (session deleted in the GUI meanwhile) → reply
   `session '<name>' no longer exists — /sessions` and clear the chat's pointer;
   LLM-side errors arrive as kind-2 → forwarded.
7. Log to `~/.wuffagent/telegram.log` (append, small rotation cap) + `eprintln!`.

**`telegram` tool** (standard 3-symbol exports, name `telegram`) — gives the
*agent* the same session control, so it can organize work (e.g. "I'll move this
to a dedicated session"):

| action | params | result |
|---|---|---|
| `start` / `stop` / `status` | — | poller lifecycle + state (token prefix only) |
| `send` | `to` (chat id), `text` | proactive bot→user message |
| `new_session` | `name` | creates + returns id (does NOT switch any chat) |
| `list_sessions` | — | `[{id, name}]` |
| `use_session` | `name` or `id` | sets the calling chat's current session (no-op if invoked outside a telegram-originated turn; then just resolves + reports) |

Auto-start: if the config file parses at `wuff_tool_create` time, start the
poller immediately (so "install → reload_plugins → talk" works with no further
interaction). `wuff_tool_host_api` stores the vtable in a `static AtomicPtr`
before the poller can use it (the loader calls it at load time; if it arrives as
null — e.g. loaded by a non-app host — the tool reports `host_api: missing` and
`start` degrades to send-only mode with a clear error on inject).

## Phases

### P0 — core: HostApi + loader plumbing ✅ (done — commit "P0: optional HostApi vtable...")
- [x] `HostApi` struct + `HOST_API_VERSION` in `tools/types` (`#[repr(C)]`).
      `HostApi` + `HostEventCallback` + `HOST_API_VERSION = 1` + `Send/Sync`;
      doc'd blocking contract. (types.rs, after `PluginTool`.)
- [x] `ToolRegistry::set_host_api(Option<NonNull<HostApi>>)` + `host_api_ptr()`
      + `host_api: AtomicPtr<HostApi>` field (null by default).
- [x] Loader: `PluginHandle::call_host_api(*const HostApi)` — `lib.get
      (b"wuff_tool_host_api")`, `catch_unwind` around the call; missing symbol
      = tool-only plugin (no call, no error). Wired in `load_one_plugin`: the
      cheap Arc handle is cloned before it moves into the `ToolEntry`, and the
      call happens only on a fresh `Loaded` outcome (re-scans keep the
      original instance's vtable — no double-init). `PluginLoadOutcome` unchanged.
- [x] Unit tests: `test_set_host_api_roundtrip` (default null, set round-trips,
      `None` clears), `test_host_api_version_and_layout` (version constant +
      repr(C) align/size guard), `test_tool_only_plugin_loads_with_host_api_set`
      (END-TO-END get-missing path using the real `hello_plugin.dll` — hello has
      no `wuff_tool_host_api`, loads + executes unchanged with a host API set,
      and a re-scan still skips). Version check itself lives plugin-side
      (compare `api.version` vs `HOST_API_VERSION`) — the constant is exported
      for that; the comparison lands in P2.
- [x] `cargo test -p wuffagent-core` green: **889 passed** (+2/2/1 integration
      targets), exit 0. `cargo check --workspace` exit 0. Note: one pre-existing
      `server::tests::attach_mode_when_port_free` llama-server port flake failed
      once in a full run but passes in isolation (unrelated to this change).
- [x] `hello_plugin` still loads — proven by the end-to-end loader test above
      (real DLL, host API set, loads + executes). Manual `reload_plugins` →
      `hello` is a UI round-trip; left for the user to confirm if desired.

**Next (P2 — telegram_plugin crate):** see P2 below.

### P1 — egui: host bridge ✅ (done — commit "P1: egui host bridge...")
- [x] Implement the vtable fns (extern "C", `std::panic::catch_unwind` around
      bodies — no panics across the boundary):
  - command queue (`std::sync::mpsc`) drained each frame in the same
    housekeeping slot as `process_pending_events`; command set:
    `Inject { sid, text }` (auto-create if missing → `start_pipeline_for_session`
    with the session's current agent), `Create { name, reply }`,
    `Resolve { query, reply }`, `Count { reply }`, `Get { i, reply }` —
    replies via one-shot `std::sync::mpsc` recv (5 s timeout) on the calling
    thread;
  - `register_event_callback` → single slot store; UI-thread event processing:
    for each pipeline `AppEvent` (StreamChunk/StreamRoundComplete/
    StreamComplete/StreamError) map to kind + copy session_id/content into
    host-owned C strings and call the cb (fast path: mpsc forward only).
- [x] Bootstrap: `registry.set_host_api(...)` before any plugin load.
- [x] `cargo check -p wuffagent-egui` + egui test suite green; unit test for the
    command queue → session store effects (create/resolve/list against a real
    store fixture).

**Outcome (P1):**
- New `wuffagent-egui/src/host_bridge.rs`: `HostCommand`/`HostReply` types,
  process-global `COMMAND_TX` (installed once by `init()`, receiver handed to
  `ChatApp` via `groups::HostBridge.rx`), the `&'static HostApi` vtable
  (`host_api()`) whose fns run on the plugin thread (byte-copy + mpsc only,
  5 s `CMD_TIMEOUT`), and `emit_event(kind, sid, payload)` with a
  `catch_unwind` around the plugin callback (a panicking plugin can't take
  down the UI frame loop; the callback stays registered).
- New `ui/host_bridge_cmd.rs` (executes on the UI thread, called from
  `process_pending_events` after event drain): `process_host_commands`
  (take-rx → try_recv loop → put back; mirrors the `process_pending_events`
  pattern), `host_inject` (auto-create + `start_pipeline_for_session` with
  the session's selected agent prompt/policy), `host_create` (mirrors the
  panel's Create: `create_session` + `SessionRuntime::create_from_config` +
  "general" default + selects & persists active session), `host_resolve`
  (exact id, else case-insensitive name, sorted-id determinism),
  `host_switch` (`switch_session`), `host_get` (sorted by id — deterministic
  across calls).
- `HostApi` gains `switch_session` (inserted before `session_count` — safe,
  no plugin compiled against v1 yet) so the desktop can follow the bot's
  active session. Core also gains `HostApi::validate(*const HostApi)` — the
  plugin-side version/layout check for the dual-link case (null + future
  version reject).
- Bootstrap wires `registry.set_host_api(Some(NonNull::from(&*host_api())))`
  + `host_bridge::init()` BEFORE `register_builtins`/`discover_plugins`;
  `AppContext`/`main.rs` plumb `host_rx` into `ChatApp::new`.
- Event mapping in `event_handler/mod.rs`: StreamChunk→0, StreamComplete→1,
  StreamError→2, StreamRoundComplete→3 (empty payload), emitted before the
  consuming `match event` (sid extracted just above).
- Tests: 4 new in `host_bridge::tests` (byte helpers incl. invalid-UTF-8 +
  NUL/truncation rules; full vtable→queue→handler→reply round-trip for
  Count/Get/Create on a handler thread; event-callback slot + user_data
  forwarding) + core `test_host_api_validate_helper`.
- Verified: `cargo test --workspace` exit 0 — wuffagent-core **890 passed**,
  wuffagent-egui **61 passed** (was 57), integration targets 2+2+1.
  `cargo check --workspace` exit 0 (only the 3 pre-existing warnings).
  Note: session-store effect logic (`host_*` fns) is covered by code review +
  the mechanism round-trip test; a full ChatApp fixture test was not added
  (ChatApp::new needs engine/server fixtures) — the P2 integration test with
  a fake Telegram endpoint exercises Inject/Create/Resolve end-to-end.

### P2 — telegram_plugin crate
- [ ] Crate skeleton (cdylib, `Cargo.toml` mirroring `hello_plugin`), exports the
      4 symbols, `telegram` tool with the full action table.
- [ ] Config + state files (load/validate, atomic write), log file, `Once`-
      guarded poller thread, blocking reqwest (client built **once** — the
      llama-integration P1 lesson), offset/dedupe state.
- [ ] Telegram calls: `getUpdates` (long-poll), `sendMessage` (plain, chunked),
      `sendChatAction`. 401/409/5xx handling per design.
- [ ] Slash-command dispatch + per-chat session map + inflight pairing + reply
      accumulation via the event callback relay.
- [ ] `README.md`: setup (BotFather token, allowlist, build, install,
      `reload_plugins`), command reference, config reference, troubleshooting
      (409, bad token, server down, shared-session semantics), the dual-linking
      logging note.

### P3 — verification, docs, close-out
- [ ] Unit tests (plugin crate): chunking (4096/4100 boundaries, CRLF), allowlist
      filter, offset dedupe, config/state parse errors, version-check degrade,
      command parsing (`/new`, `/use` name vs id, unknown).
- [ ] Integration test with a **fake Telegram server** (in-test `TcpListener`
      serving canned `getUpdates`/`sendMessage` JSON via `api_base`) asserting:
      free text → inject round trip; `/new` → create + switch; `/use <id>` →
      switch; `/sessions` lists both.
- [ ] Manual E2E (real bot token — **ask the user to run this**): message the
      bot (default session), `/new fix-thing`, `/sessions`, `/use <a GUI-created
      session>`, verify: replies in Telegram, sessions visible/switchable in the
      GUI, runs in the fleet dashboard.
- [ ] Main README: short "Plugins → Telegram bot" section; `git commit`; save a
      memory entry (agent:wuffagent) with the ABI-extension shape + command set.

## Risks / mitigations

| Risk | Mitigation |
|---|---|
| Callback invoked mid-frame does slow work → UI jank | cb contract: forward to mpsc only; host-side copy is a small C-string; review in P1 |
| vtable call from the UI thread → deadlock on the command reply | documented blocking contract; only call sites are the poller thread + tool execution (pipeline thread); assert with a thread check + debug log |
| `/use` mid-turn misroutes the old turn's trailing events | inflight `(chat_id, session_id)` pair recorded at send time; events matched against it, not against the live current session |
| Session deleted in the GUI while a chat points at it | inject/resolve failure → friendly reply + pointer cleared (state file updated) |
| Two pollers (two app builds / dev + release) → 409 storm | 30 s backoff + `status` flag; documented |
| Token leaked via `status` output | `status` prints only token prefix `1234…` |
| Bot sessions grow unbounded (context) | normal sessions — existing trimming/context-overflow machinery applies unchanged |
| Plugin built against older core (vtable layout drift) | separate `HOST_API_VERSION` + degrade to send-only with clear error; same stale-DLL philosophy as `PLUGIN_ABI_VERSION` |
| `extern "C"` boundary panic aborts the app | `catch_unwind` in both host fns; plugin cb never allocates across the boundary beyond the mpsc send |
| `/new` name collision (GUI already has that name) | names are not unique in the store — allowed; `/use` resolves case-insensitive, first match wins (documented) |

## Explicitly out of scope (follow-ups)

- Rich formatting (MarkdownV2 with escaping), images/voice in/out, group topics,
  Telegram command menu (`setMyCommands` — the text prefixes already work).
- Per-side turn locking for shared sessions (bot + GUI driving one session
  concurrently) — MVP relies on abort-on-new-send.
- Plugin *unload* (ABI-wide, not bot-specific) — `stop` covers the practical need.
- Multi-bot configs (one token per plugin instance).
