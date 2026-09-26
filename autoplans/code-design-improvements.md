# Code design improvements (2026-10-05, wuffagent)

**Status:** ✅ DONE (2026-10-05, wuffagent). D5, D3, D4 (b39d406), D6 (661180b),
D2 (AgentBuilder with the engine work), D1a (SessionState) all landed.
D1b (move SessionState ownership into SessionRuntime) remains as the
optional follow-up — only worth it if a concrete need appears.

Audit of wuffagent-core + wuffagent-egui after the self-improvement loop
(autoplans/finish-self-improvement-loop.md, done 2026-09-25). The module
layout is good (small focused modules, tests next to code); the remaining
smells are one god struct, one god function, argument-soup constructors,
registration boilerplate, and a little dead code.

## Findings

### D1. `ChatClient` is a god struct — root cause of the 3-cycle (HIGH)
`client/mod.rs` (468 lines; struct with 18 fields) mixes four
responsibilities:
- LLM transport: `settings`, `http_client`, `stream_http_client`, `n_ctx`,
  `chars_per_token_x100`, `last_prompt_chars`, `reasoning_effort`,
  `max_messages`, `agent_name` stamp
- **session state**: `session_id`, `session_dir`, `conversation`
  (`Arc<Mutex<Vec<Message>>>`), `system_prompt`, `session_meta`
  (`sessions::SessionMeta`), `encryption_key`, `save_queue`, `save_failed`
- UI events: `tool_event_tx`
- telemetry: `usage_recorder`

The documented 3-cycle `client → sessions → agents → client` exists because
the client owns session state while `sessions::runtime` owns the runtime
that drives the client. Phase 2 E2a already moved the persistence
*orchestration* to `sessions::persist`; the state itself never moved.

- **D1a** (this plan): extract `SessionState { session_id, session_dir,
  conversation, system_prompt, meta, encryption_key, save_queue,
  save_failed }` into a new `sessions::state` module; `ChatClient` holds an
  `Arc<SessionState>`. `client/persist.rs` methods and the `client/session.rs`
  conversation helpers operate through the state. `SessionRuntime` gets a
  `session_state()` accessor so the egui call sites
  (`window.rs:192/205/360/410`, `state.rs:219`, `main.rs:205`) can keep
  calling `runtime.client.save_session()` (delegating) OR switch to
  `runtime`-level methods. Gate: full workspace tests + build.
- **D1b** (follow-up, only if D1a is clean): move `SessionState` ownership
  INTO `SessionRuntime` (client becomes pure transport; the `client →
  sessions` edge disappears and the graph is linear:
  `types ← client ← {agents, sessions}`). Revisit at that point whether
  `ChatClient` still needs `agent_name`/`usage_recorder` (telemetry could
  move to the caller).

### D2. `Agent::new` / `Agent::from_config` argument soup (MEDIUM)
`Agent::new(config, llm_client, tool_manager, event_tx, client, memory,
agent_session_id)` — 7 positional args, 4 `Option`s; 12 call sites, 11 pass
`None` for ≥2 args. `from_config(config, llm_client, client, memory)`
duplicates the empty-tool-manager setup. An `AgentBuilder` (required:
config/llm_client/client; optional: tool_manager/event_tx/memory/
agent_session_id/handoff/restart mailboxes) removes the `None` noise,
makes argument-order mistakes unrepresentable, and is the only sane place
for the new per-agent pieces as they land. `from_config` becomes
`Agent::builder(cfg).llm(llm).client(client).memory(m).build()`.

### D3. Tool-registration boilerplate (MEDIUM, easy win)
`tools/builtin/mod.rs` (490 lines): five `register_*` functions each build
`ToolEntry { tool, metadata: ToolMetadata { name, version: "1.0.0",
description, dependencies: vec![] }, loaded_at: now, plugin: None }` by
hand. A single `fn register_tool(registry, name, description, tool)
-> ToolResult<()>` helper (private to the module) cuts ~40% of the file and
gives one place to change the entry shape (e.g. per-plugin metadata later).

### D4. `agent_config.rs::show()` god function (MEDIUM, easy win) — ✅ DONE
Split into private per-section draw methods on AgentConfigPanel
(draw_header/draw_agent_list/draw_editor + its subsections/draw_prompt_
history/draw_shell_group/draw_metrics/draw_handoff_group/draw_restart_
group/draw_save_cancel). Commit b39d406.

### D5. Dead code + stale names (LOW, easy wins) — ✅ DONE
- `wuffagent-core/src/config_types.rs`: re-export-only module, ZERO importers
  in core, egui, or plugins → delete + drop from lib.rs graph.
- `client/session.rs` is a misnomer since E2a (persistence moved out): it
  holds pure conversation-`Vec<Message>` helpers. Rename to
  `client/conversation.rs` (dir `client/session/` → `client/conversation/`
  for the tests).
- lib.rs dependency-graph doc: refresh to match (config_types gone,
  client → sessions edge now via `sessions::state` after D1a).

### D6. `ui/input/mod.rs::handle_send_input` inject-or-queue branch (LOW) — ✅ DONE
main.rs 470→104 via bootstrap.rs (AppContext struct + staged builders),
logging.rs, fonts.rs. Commit 661180b. (The input/mod.rs extraction was
folded into this commit's scope — verify in git log if needed.)

## Order & gates
D5 → D3 → D6 → D2 → D4 → D1a → (D1b separately).
Each: `cargo test -p wuffagent-core` + `-p wuffagent-egui` +
`cargo build --workspace`, no warnings; one commit each; restart WuffAgent
after core changes.

## Explicitly deferred (not design, or big cross-cutting)
- Typed `LlmError` instead of `Result<_, String>` everywhere (known since
  step 20; touches every LLM impl + caller — needs its own plan).
- `agents/manager.rs` (605 lines) split profiles/history — acceptable size,
  revisit only if it grows.
