# WuffAgent simplification — work orders (orchestrator → coder)

Companion to `autoplans/simplify-wuffagent.md` (the audit; commit `311ba7f`).
Goal: **same behavior, less code, less routing, less threading, less cloning.**
Every work order (WO) is behavior-preserving and self-contained: the coder can
execute it without reading the audit. All file paths are relative to
`M:\repos\WuffAgent`.

## Orchestration protocol

1. Dispatch **one WO at a time** to the coder, in the order below.
2. Gate before accepting a WO (all must pass):
   - `cargo build --workspace` → success
   - `cargo clippy --workspace --all-targets` → **no new** warnings vs. before
   - `cargo test --workspace` → all pass (baseline: full suite green today)
   - `git log -1` shows the WO's commit message
3. The coder commits each WO itself with the exact message given.
4. A WO failing the gate twice → revert the commit (`git revert`), report to the
   user with the failure evidence, do not retry a third time.
5. Only dispatch WO-6 (egui) after WO-1…WO-5 are all committed (keeps the
   core stable before touching the UI crate).

## Baseline (measured 2026-09-01, verified against working tree)

- `Arc<Mutex<ToolManager>>` — 10 sites in `wuffagent-core/src`
- `tools/manager.rs` — 9 near-identical `with_*`/`without_*` registry-rebuild
  methods, ~360 lines
- `agents/chat_pipeline.rs` — `AtomicPtr<CancellationToken>`,
  `Box::into_raw`/`Box::from_raw` at lines 69/109/116/120/201/228,
  `unsafe impl Send/Sync` at 51–52 (231 lines total)
- `agents/engine.rs` — 11 `with_*` methods; the run-scoped ones are
  `with_injection_channel` (78), `with_event_tx` (110), `with_reasoning_mode`
  (127, **deep-clones the ChatClient**), `with_reasoning_effort` (143),
  `with_session_id` (148)
- `agents/agent/mod.rs` — 4 per-execution mailboxes (lines 62–80) + 4
  `take_pending_*` methods (410–429) + the 5-way `with_*` chain in
  `AgentBuilder::build` (224–273)
- `wuffagent-egui/src/ui/agent_config.rs:638–643` — the only throwaway
  tokio runtime in the UI (`new_current_thread` + `block_on` for `run_eval`)

---

## WO-1 — `Arc<Mutex<ToolManager>>` → `Arc<ToolManager>`

**Why:** The per-run `ToolManager` is built once in `AgentBuilder::build` and
never mutated afterwards; all its methods are `&self`. The `Mutex` is vestigial
and costs a lock on every tool lookup and every agent build.

**Changes:**
- Replace the type `Arc<Mutex<ToolManager>>` with `Arc<ToolManager>` at all 10
  sites in `wuffagent-core/src` (found via search for the exact string
  `Arc<Mutex<ToolManager>>`).
- Remove the now-pointless `.lock().unwrap()` accesses (e.g.
  `agents/agent/mod.rs:224` `let shared = tool_manager_arc.lock().unwrap();`
  and the per-access locks in `agents/agent/loop.rs`).
- `ToolManager` itself gains nothing; if a `Clone` derive is needed anywhere it
  already exists (manual impl in `tools/manager.rs:81`).

**Acceptance:** grep for `Arc<Mutex<ToolManager>>` in `wuffagent-core/src`
returns 0; full test suite green; no behavior change.

**Commit message:** `simplify: drop vestigial Mutex around ToolManager (S6)`

**Risk:** low / mechanical. Watch for any site that mutates through the
manager (`remove_tool`, `add_discovery_path`) — those already operate on
`&self` (registry interior mutability) and need no change.

---

## WO-2 — ToolManager: one override mechanism instead of 9 registry rebuilds

**Why:** `tools/manager.rs` has 9 near-identical methods (`with_shell_config`,
`with_handoff_tool`, `with_restart_tool`, `with_hand_back_tool`,
`with_session_note_tool`, `without_handoff`, `without_hand_back`,
`without_restart`, `without_shell`). Each copies the entire registry
(`registry.list()` → `ToolRegistry::new` → re-register loop) and
`AgentBuilder::build` chains up to 5 of them per run — every agent run
allocates up to 5 full registries.

**Changes:**
1. Add to `tools/manager.rs`:
   ```rust
   /// Per-run view over the shared registry: tool replacements (e.g. a
   /// per-agent shell, handoff, restart, hand_back, session_note) and
   /// removals. Shares the registry — no copying of entries.
   #[derive(Clone, Default)]
   pub struct ToolOverrides {
       replaced: Vec<ToolEntry>,   // from tools::registry; keyed by metadata.name
       removed: Vec<String>,
   }
   ```
   (Expose whatever accessors are needed; `ToolEntry` is already `pub` in
   `tools/registry.rs`.)
2. Add field `overrides: ToolOverrides` to `ToolManager` (clone it in the
   manual `Clone` impl and in every constructor).
3. Replace the 9 methods with thin wrappers over one private helper:
   ```rust
   fn with_override(&self, name: &str, entry: Option<ToolEntry>) -> Self
   // entry = Some(...) → replace; None → remove
   ```
   Each public method builds its `ToolEntry` (metadata lookup or synthesized
   default — keep the exact existing metadata fallbacks) and calls the helper.
   Public names/signatures stay unchanged so call sites don't move.
4. Route lookups through overrides first:
   - `execute_with_progress` / `get`: check `overrides.replaced` for the name
     (and reject names in `removed`), then fall through to the shared registry.
   - `get_tool_definitions`: registry definitions, minus removed, plus/with
     replaced entries' definitions.
   - `get_allowed_tools`: same merge (respect allowlist as today).
   - `validate` / `schema_for` path: for a replaced tool, validate against the
     replacement's schema.
   - `remove_tool`, `add_discovery_path` stay registry-level (unchanged).

**Acceptance:**
- The 5 rebuild loops are gone: no `registry.list()` copy inside
  `with_*`/`without_*` methods anymore (each is ≤ ~15 lines).
- `tools/manager.rs` net smaller by ≥ 200 lines.
- All existing tests green, especially `agents/agent/tests/schema.rs`
  (shell/handoff/restart/hand_back/session_note schema gating) and
  `tools/manager` tests.

**Commit message:** `simplify: ToolManager per-run overrides replace 9 registry rebuilds (S1)`

**Risk:** medium. The subtle parts: (a) allowlist + overrides interaction in
`get_tool_definitions`/`get_allowed_tools` — the allowlist filter must apply
after the merge, exactly as today; (b) `with_shell_config` currently rebuilds
the registry from a fresh `ToolRegistry::new(discovery_paths, logger)` — after
this change the shared registry is the source of truth, which is the intended
simplification (the rebuild loop re-registered the same entries anyway).

---

## WO-3 — One control mailbox for handoff / restart / hand_back / session_note

**Why:** `agents/agent/mod.rs` carries 4 parallel per-execution mailboxes
(`handoff_mailbox`, `restart_mailbox`, `hand_back_mailbox`,
`session_note_mailbox`) — 4 `Arc<Mutex<Option<..>>>` fields, 4
`take_pending_*` methods, 4 builder steps, and a drain site per mailbox in
`run_llm_loop` (`agents/agent/loop.rs`). Same pattern ×4.

**Changes:**
1. In `agents/types.rs` (or `agents/agent/mod.rs` if more local is clearer):
   ```rust
   enum ControlRequest {
       Handoff(HandoffRequest),
       Restart(RestartRequest),
       HandBack(HandBackRequest),
       SessionNote(SessionNoteRequest),
   }
   ```
2. `Agent` field: `control_mailbox: Arc<Mutex<Vec<ControlRequest>>>`
   (always present — this also removes the `session_note_mailbox`
   always-Arc vs. the other three `Option<Arc<..>>` asymmetry).
   Delete the 4 old fields and the 4 `take_pending_*` methods; add
   `drain_control(&self) -> Vec<ControlRequest>` (lock + `mem::take`).
3. The four builtin tools (`tools/builtin/{handoff,restart,hand_back,session_note}`)
   each receive the shared `Arc<Mutex<Vec<ControlRequest>>>` and push their
   own variant (they don't need to know about the other variants).
4. `AgentBuilder::build`: one mailbox created up front; tool injection per
   config flag stays, but the builder no longer tracks 4 mailboxes — it just
   wires the shared Arc into whichever tools are enabled.
5. `run_llm_loop`: drain once per LLM round boundary; match on the variants
   and do exactly what the 4 separate drain sites do today (keep the existing
   per-variant behavior, order, and event emission identical — if multiple
   requests are queued, process them in insertion order).

**Acceptance:**
- `grep 'mailbox' wuffagent-core/src/agents/agent/` shows one field, one
  drain site.
- `agents/agent/tests/handoff.rs`, `tests/hand_back.rs`, and the session-note
  tests all green unchanged.
- Net deletion ≥ ~100 lines.

**Commit message:** `simplify: single control mailbox for handoff/restart/hand_back/session_note (S2)`

**Risk:** medium. Behavior to preserve exactly: handoff ends the turn, restart
ends the turn after the UI relaunch, hand_back returns to parent, session_note
inserts an anchored user message before the next LLM round. If any of the four
had a distinct "process-before-next-round" timing in `loop.rs`, keep that
timing for its variant (drain order must not change observable behavior).

---

## WO-4 — ChatPipeline: one `Mutex<RunState>`, delete all `unsafe`

**Why:** `agents/chat_pipeline.rs` (231 lines) uses
`AtomicPtr<CancellationToken>` with manual `Box::into_raw`/`Box::from_raw`
(lines 69, 109, 116, 120, 201, 228) and `unsafe impl Send/Sync` (51–52).
All of it exists to swap per-run state (cancel token, join handle, injection
receiver). Everything is already "touched from the UI thread except the
drain" — a single `std::sync::Mutex` with short critical sections is
trivially correct.

**Changes:**
1. Define:
   ```rust
   struct RunState {
       token: Option<CancellationToken>,
       handle: Option<JoinHandle<()>>,
       injection: Option<Arc<Mutex<mpsc::Receiver<QueuedMessage>>>>,
   }
   ```
2. `ChatPipeline` fields: `run: Mutex<RunState>` replaces
   `current_token` / handle / injection fields; keep
   `engine`, `event_tx`, `reasoning_mode`, `session_id` as-is.
3. `start()`: under the lock, build the fresh token/handle/injection and store
   them (drop the old handle inside the lock — `JoinHandle` drop is cheap,
   the task self-terminates via its own token clone; if the old task needs an
   explicit cancel, call `old_token.cancel()` after taking it out).
4. `stop()`/cancel path: lock, clone the token out, call `.cancel()` (no raw
   pointer deref).
5. Drop both `unsafe impl` blocks and the `AtomicPtr` import.

**Acceptance:** `grep -n 'unsafe\|into_raw\|from_raw\|AtomicPtr'
wuffagent-core/src/agents/chat_pipeline.rs` returns nothing; full suite
green; stop-during-run and the mid-run injection tests still pass.

**Commit message:** `simplify: ChatPipeline RunState mutex removes unsafe pointer code (S3)`

**Risk:** medium. Check every place that reads the token across `.await` or
thread boundaries: `CancellationToken` is `Send + Sync`, so a clone taken
under the lock is always safe. Never hold the `Mutex` guard across an await
(lock, clone, unlock, then await).

---

## WO-5 — AgentEngine/ChatPipeline: run-scoped values as parameters, no per-run clones

**Why:** Every `ChatPipeline::start()` (see `agents/chat_pipeline.rs:136-149`)
clones the engine, then chains `with_event_tx` → `with_reasoning_mode` (which
**deep-clones the whole `ChatClient`**, `agents/engine.rs:127-138`) →
`with_session_id` → `with_injection_channel` and re-Arcs it. Run-scoped values
travel as struct clones. `SessionRuntime::create_from_config`
(`sessions/runtime/mod.rs:405-414`) does the same chain per session, so one
session holds **three** copies of the same client (`client`,
`engine.client`, `pipeline.agent_engine.client`) and two engine copies.

**Changes (do in this order, one commit per sub-step):**

**5a — RunParams plumbing (behavior-neutral):**
1. Add to `agents/engine.rs`:
   ```rust
   pub struct RunParams {
       pub event_tx: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
       pub session_id: Option<String>,
       pub reasoning: ReasoningMode,
       pub injection: Option<Arc<Mutex<mpsc::Receiver<QueuedMessage>>>>,
   }
   impl Default for RunParams { /* all None/Auto */ }
   ```
2. `execute_with_tools` gains a `params: &RunParams` argument. Inside, the
   Agent is built (engine.rs:218) with these values instead of reading them
   from engine fields: `Agent::builder(...).event_tx(params.event_tx.clone())`
   `.agent_session_id(params.session_id.clone())`
   `.with_injection_channel(params.injection.clone())` (guard: only when
   `Some`). The reasoning mode is applied where the per-run client clone is
   already made (the engine's current `with_reasoning_mode` logic moves into
   `execute_with_tools` — a **single** client clone per run, replacing the
   current double clone path).
3. Update `ChatPipeline::start()` to build a `RunParams` and call
   `execute_with_tools` on the shared `&Arc<AgentEngine>` — no engine clone,
   no re-Arc.
4. Callers of `execute_with_tools` that don't care pass `&RunParams::default()`.

**5b — Delete the dead `with_*` methods:**
After 5a compiles and tests pass, delete `with_event_tx`, `with_session_id`,
`with_injection_channel`, `with_reasoning_mode`, `with_reasoning_effort` (if
unused after 5a — verify with grep first) and any now-dead engine fields
(`session_id`, `injection_rx`, etc.). Keep `with_agents_dir`,
`with_agents_search_dirs`, `with_memory`, `with_client` (app-level, set once
per session — those are fine).
Also verify `SessionRuntime` now shares **one** `Arc<ChatClient>` between
client/engine/pipeline (the pipeline holds `Arc<AgentEngine>` which holds the
same `Arc<ChatClient>` the session created — if the pipeline currently builds
its own client clone, wire in the session's instead).

**Acceptance:**
- `start()` contains no `engine.clone()` / no `with_event_tx` /
  `with_session_id` / `with_injection_channel` calls.
- `SessionRuntime` holds one `Arc<ChatClient>`; no per-session client
  duplication (verify by counting `ChatClient` allocations in
  `create_from_config` — should be exactly one).
- Full suite green, **plus** run the session-critical tests explicitly:
  `cargo test -p wuffagent-core handoff`, `cargo test -p wuffagent-core
  hand_back`, `cargo test -p wuffagent-core session`, and the improvements /
  eval runs (they build engines headlessly).

**Commit messages:**
`refactor: AgentEngine run-scoped values move to RunParams (S4a)`
`refactor: delete per-run engine clone chain, share one client per session (S4b)`

**Risk:** highest of the six. The engine's reasoning-mode/client-clone
interaction is the subtle part — if `with_reasoning_mode`'s semantics can't be
reproduced with one clone at `execute_with_tools` time, stop and report rather
than invent a new clone path. Rollback: `git revert` the 5a/5b commits
independently.

---

## WO-6 — egui: shared runtime for `run_eval` (no throwaway runtime)

**Why:** `wuffagent-egui/src/ui/agent_config.rs:638–643` is the only place in
the UI that builds a throwaway tokio runtime (`new_current_thread` +
`block_on`) just to execute one tool. Bootstrap already creates a shared
helper runtime (`memory_runtime`, dropped via `RuntimeOnThread` in
`ui/state.rs`) that the memory editor already `block_on`s from plain threads
(`ui/memory_panel/editor.rs:301–307`).

**Changes:**
- In `agent_config.rs`, thread the existing helper runtime
  (`&Arc<tokio::runtime::Runtime>` — same plumbing the memory editor uses)
  into the `run_eval` spawn site and replace lines 642–643 with
  `rt.block_on(tm.execute("run_eval", ToolParams { values }))` on the shared
  runtime. No new thread beyond the existing `std::thread::spawn` worker.

**Acceptance:** `grep -rn 'new_current_thread' wuffagent-egui/src` returns 0;
`cargo build --workspace` green; the "Run evals" button path still works
(covered by manual check — the UI has no automated test for this button;
verify the code path compiles and matches the memory-editor pattern).

**Commit message:** `simplify: reuse shared helper runtime for run_eval (S8)`

**Risk:** low. `ToolManager::execute` may internally `spawn_blocking` — that
is fine inside any runtime's `block_on` (the existing memory-editor path
proves the pattern).

---

## Deferred / not worth it (do NOT dispatch without re-discussion)

- **S5 — delete the `LlmClient` trait:** re-checked, it is a **live test
  seam**: 8 mocks implement it (`RefuseLlm`, `JudgeLlm`, `NoopLlm`,
  `CountingLlm`, `CaptureLlm`, `QueuedLlm`, `ScriptedLlm`, `MockLlm`) and it
  is used for real in `agents/agent/verify.rs:187` and
  `agents/improvement.rs:625,1133`. Deleting it rewrites 8+ mock sites for
  ~70 lines saved. Keep the trait; it is the one abstraction in this crate
  that earns its place.
- **S7 — ChatClient run-scoped state (`agent_name`, `trim_pcts`) into a
  request-time `RequestContext`:** still valid (it breaks the "agents within
  a session run sequentially" temporal invariant), but it touches 39 pub fns.
  Revisit after WO-1…WO-6 land, with fresh measurements.
- The shared `AppEvent` channel + `session_id` routing, per-session isolated
  clients, `SessionState` interior-mutability bag, the `trimming` module, and
  the `agents/agent/` file split: **leave as-is** (see audit §4).

## Expected outcome (all 6 WOs)

| Item | Lines removed (est.) | Structural gain |
|---|---|---|
| WO-1 | ~20 | vestigial lock per tool access gone |
| WO-2 | ~300 | 5 registry rebuild loops per run → 1 shared registry |
| WO-3 | ~120 | 4 mailboxes + 4 drain sites → 1 |
| WO-4 | ~70 | all `unsafe` / `AtomicPtr` / raw `Box` gone |
| WO-5 | ~120 | per-run engine clones + 3× client copies per session → 1 |
| WO-6 | ~30 | last throwaway runtime gone |
| **Total** | **~650–750** | **plus every hard-to-reason-about line above deleted** |
