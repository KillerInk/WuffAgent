# WuffAgent simplification audit

Date: 2026-07-09 (approx.) — request: "code design is too complex — same behavior, less code,
less routing, less threading, less cloning."

## 1. Current state (measured)

| Area | Number |
|---|---|
| `wuffagent-core/src` | 212 files / 48,982 lines (32,495 non-test) |
| `wuffagent-egui/src` | 42 files / 12,975 lines |
| `Arc::new` | 77 |
| `.clone()` | 364 |
| `Mutex` refs | 158 |
| `mpsc::` | 23 |
| `tokio::spawn` in core | 6 |
| `std::thread::spawn` in egui | 8 (several build a throwaway `new_current_thread` runtime) |

Top-level core modules by size: `tools` 17.9k, `agents` 11.9k, `trimming` 5.0k, `memory` 4.4k,
`client` 3.7k, `config` 1.9k, `sessions` 2.0k.

The module graph (documented in `lib.rs`) is already tidy — no 2-cycles, one deferred 3-cycle.
The remaining complexity is **inside** modules: per-run cloning, registry rebuilding, mailbox
proliferation, and one `unsafe` pipeline. Findings below are ordered by value/risk.

## 2. Hotspots (with evidence)

### H1 — ToolManager rebuilds the entire registry up to 5× per agent run (cloning)
`tools/manager.rs`: 9 near-identical methods — `with_shell_config`, `with_handoff_tool`,
`with_restart_tool`, `with_hand_back_tool`, `with_session_note_tool`, `without_handoff`,
`without_hand_back`, `without_restart`, `without_shell`. Each copies **all** registry entries
(`registry.list()` → new `ToolRegistry::new` → `register` loop, ~40 lines each, ~360 lines total).
`AgentBuilder::build` (agents/agent/mod.rs:224-273) chains up to 5 of them per run, so every
agent run allocates up to 5 full registries. This is the single biggest clone hotspot.

### H2 — Four parallel per-execution mailboxes (routing)
`agents/agent/mod.rs:62-80`: `handoff_mailbox`, `restart_mailbox`, `hand_back_mailbox`,
`session_note_mailbox` — four `Arc<Mutex<Option<Request>>>` with four `take_pending_*` methods
(410-429) and four `with_*_tool` builder steps. Same pattern ×4.

### H3 — ChatPipeline: `unsafe` + raw pointers + double-mutex injection channel
`agents/chat_pipeline.rs` (231 lines): `AtomicPtr<CancellationToken>` with manual
`Box::into_raw`/`Box::from_raw` in 3 places, `unsafe impl Send/Sync`, and the injection channel
as `Mutex<Option<Arc<Mutex<Receiver>>>>` (outer Mutex = "UI thread only", inner Mutex =
task/cancel drain serialization). All of it exists to swap per-run state (token, handle, rx).

### H4 — AgentEngine smuggles run-scoped state via per-run clones
`agents/engine.rs` + `chat_pipeline.rs:136-149`: every `start()` clones the engine, re-Arcs it
after `with_event_tx` → `with_reasoning_mode` (which **deep-clones the whole ChatClient** —
engine.rs:127-138) → `with_session_id` → `with_injection_channel`. Run-scoped values
(event_tx, session_id, reasoning mode, injection rx) travel as struct clones instead of
parameters. `SessionRuntime::create_from_config` does the same chain per session
(sessions/runtime/mod.rs:405-414), so one session holds **three** copies of the same client
(`client`, `engine.client`, `pipeline.agent_engine.client`) and two engine copies.

### H5 — `LlmClient` trait with exactly one implementation (routing)
`llm.rs`: `pub trait LlmClient` + `ChatClientAdapter { client: ChatClient }`. `impl LlmClient
for ChatClientAdapter` is the only impl in the crate. `AgentEngine` and `Agent` therefore carry
both `llm_client: Arc<dyn LlmClient>` and `client: Arc<ChatClient>` — two handles to the same
object for non-streaming calls.

### H6 — `Arc<Mutex<ToolManager>>` — vestigial Mutex
The per-run `ToolManager` is built once in `AgentBuilder::build` and never mutated afterwards;
`ToolManager` methods are all `&self`. Yet the type is `Arc<Mutex<ToolManager>>` in 10 places
(Agent, AgentEngine, builder, UI), locking on every access (loop.rs:91, agent/mod.rs:224).
`Arc<ToolManager>` suffices (the registry's own interior mutability already covers plugin loads).

### H7 — ChatClient facade: 15 fields, 8 of them `Arc<Mutex<..>>`/atomics (bigger)
`client/mod.rs:112-178`. Beyond `SessionState`, per-RUN state is stamped onto the shared handle:
`agent_name`, `trim_pcts` ("agents within a session run sequentially, so the stamp is current at
request time" — a temporal invariant, not a structural one). 39 pub fns. Candidate for moving
run-scoped state to an explicit request-time context (larger refactor; do after H1-H6).

### H8 — egui: throwaway threads + runtimes per operation
`ui/agent_config.rs:638`, `ui/improvements/draw.rs:185,213`, `ui/mod.rs:430,509`,
`ui/editor.rs:301`: each spawns a thread, builds a `new_current_thread()` tokio runtime,
`block_on`s, drops it. `RuntimeOnThread` (state.rs) exists only because dropping a runtime in
tokio context panics. One shared helper runtime (like `memory_runtime`) removes all of it.

## 3. Proposed simplifications (same behavior, ordered)

All are behavior-preserving; the 16.5k-line test suite is the safety net. Do them as separate
commits/steps, `cargo test` after each.

### S1 — ToolManager: overrides instead of registry rebuilds (kills H1)
Add a small per-run view to `ToolManager`:
```rust
pub struct ToolOverrides { replacements: Vec<(String, Arc<dyn Tool>)>, removals: Vec<String> }
impl ToolManager {
    pub fn with_overrides(&self, o: &ToolOverrides) -> Self; // shares registry, no copy
}
```
`get()` checks overrides first, then the shared registry; `to_tool_definitions()`/`list()`
merge; `remove_tool` unchanged. Replace all 9 methods with thin wrappers (or delete the ones
only used in one place). `AgentBuilder::build` becomes one `ToolOverrides` construction.
**Saves ~300-360 lines, removes per-run O(N) registry copies (up to 5×), one code path.**

### S2 — One control mailbox for handoff/restart/hand_back/session_note (kills H2)
```rust
enum ControlRequest { Handoff(HandoffRequest), Restart(RestartRequest),
                      HandBack(HandBackRequest), SessionNote(SessionNoteRequest) }
// Agent: control_mailbox: Arc<Mutex<Vec<ControlRequest>>>
// drain_control(&self) -> Vec<ControlRequest>  — matched once in run_llm_loop
```
One mailbox, one drain site, tools write via the same `Arc` (they each only know their variant).
**Saves ~120-150 lines, 4 patterns → 1, and the `session_note_mailbox` Option-vs-Arc asymmetry
goes away (the field becomes always-present, tool injection stays optional).**

### S3 — ChatPipeline: one `Mutex<RunState>`, no `unsafe` (kills H3)
```rust
struct RunState {
    token: CancellationToken,          // plain field; task keeps its own clone
    handle: Option<JoinHandle<()>>,
    injection: Option<Arc<Mutex<mpsc::Receiver<QueuedMessage>>>>,
}
// ChatPipeline { engine: Arc<AgentEngine>, event_tx, reasoning_mode, session_id,
//                run: Mutex<RunState>, injection_tx: Mutex<mpsc::Sender<QueuedMessage>> }
```
Everything already "only touched from UI thread except the drain", so a single std Mutex with
short critical sections is correct and trivially `Send+Sync` (drop both `unsafe impl`s and the
`AtomicPtr`). **Saves ~60-80 lines + all raw-pointer code.**

### S4 — AgentEngine: pass run params, stop cloning (kills H4)
```rust
pub struct RunParams<'a> { pub event_tx: Option<&'a mpsc::Sender<AppEvent>>,
    pub session_id: Option<&'a str>, pub reasoning: ReasoningMode,
    pub injection: Option<Arc<Mutex<mpsc::Receiver<QueuedMessage>>>> }
pub async fn execute_with_tools(&self, req, sys, policy, image, cancel, params: &RunParams)
```
Engine holds immutable app-level deps; run-scoped values are arguments. Delete
`with_event_tx` / `with_session_id` / `with_injection_channel` / `with_reasoning_mode` (and the
per-run `ChatClient` deep clone for reasoning — the effort can be applied to the client the
agent builder already clones). `SessionRuntime` can then hold one `Arc<ChatClient>` shared by
client/engine/pipeline (the engine's `client` becomes `Arc<ChatClient>` taken at construction).
**Kills the 3-way client duplication per session and the per-run engine re-Arc dance.**

### S5 — Delete the `LlmClient` trait layer (kills H5)
Only impl is `ChatClientAdapter(ChatClient)`. Replace `Arc<dyn LlmClient>` with
`Arc<ChatClient>` in engine/agent/memory call sites (the 2-3 non-streaming calls used),
delete `llm.rs`. If a headless/mock client is wanted for tests later, re-introduce a trait
where the fake is actually needed. **Saves ~70 lines + one handle per engine/agent.**

### S6 — `Arc<Mutex<ToolManager>>` → `Arc<ToolManager>` (kills H6)
Mechanical, 10 sites, removes a lock per tool lookup and per agent build.

### S7 — ChatClient run-scoped state → request-time context (H7, do later)
Move `agent_name` + `trim_pcts` (and possibly `tool_event_tx`) from the shared handle into a
small `RequestContext` passed at request time (the agent already knows both). Breaks the
"agents run sequentially" temporal invariant. Bigger surface (39 fns); tackle after S1-S6 prove
the pattern.

### S8 — One shared helper runtime in egui (H8)
Bootstrap already builds `memory_runtime: Arc<Runtime>` (dropped via `RuntimeOnThread`).
Reuse it for agent-config save, improvement runs, editor ops: `runtime.spawn(...)` +
`rx.recv()` for results (or `block_on` from the plain helper threads that already exist).
Delete the per-call `Builder::new_current_thread()` sites and their own drop handling.

## 4. What is NOT worth simplifying (checked, leave as-is)

- **Single shared `AppEvent` channel + `session_id` routing**: non-displayed sessions must keep
  updating their `chat_state` while the user watches another; per-session channels would need
  N pumps. 17 variants is a normal UI event surface. Keep.
- **Per-session isolated `ChatClient`/conversation**: required for parallel sessions (documented
  race fix in `SessionRuntime`). Keep.
- **`SessionState` as `Arc<Mutex<..>>` bag**: it already solved the "cloned client drifts"
  problem (memory: D1a). Keep.
- **`trimming` module size**: domain logic (file state, summarizer, brief), not indirection.
- **`agents/agent/` 10-file split**: deliberate size-rule split; merging files back only moves
  lines around.

## 5. Expected outcome

| Item | Lines removed (est.) | Structural gain |
|---|---|---|
| S1 overrides | ~320 | 5 rebuild loops → 1 merge; no per-run O(N) copies |
| S2 control mailbox | ~130 | 4 mailboxes/drain sites → 1 |
| S3 pipeline RunState | ~70 | `unsafe`, `AtomicPtr`, raw Box gone |
| S4 run params | ~120 | per-run engine/client clones gone; 3 copies → 1 |
| S5 LlmClient | ~70 | 1-impl trait + double handle gone |
| S6 ToolManager Arc | ~20 | vestigial lock per access gone |
| S8 helper runtime | ~60 | 5 throwaway thread+runtime pairs → shared runtime |
| **Total** | **~800-900** | **plus the conceptual wins above** |

Roughly 2.5-3% of non-test core lines, but the removed lines are disproportionately the
hard-to-reason-about ones (unsafe, registry rebuilds, clone chains, temporal invariants).

## 6. Execution order

1. S6 (mechanical, tiny) → 2. S1 → 3. S2 → 4. S3 → 5. S4 → 6. S5 → 7. S8 → (later) S7.
Each step: implement, `cargo test` full suite, commit. S1-S5 are core-only; S8 is egui-only.
S4 is the one that touches `SessionRuntime` — run the handoff/sub-session tests specifically
(agents/agent/tests/handoff.rs, hand_back.rs).
