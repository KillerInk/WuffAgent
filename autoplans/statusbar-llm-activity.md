# Plan: Status bar shows live LLM activity (prompt-processing + token-generation) for ALL LLM calls, incl. background

**Status:** in progress — design finalized from investigation, ready to implement (next step: 1. core activity module)
**Date:** 2026-07-19
**Owner:** wuffagent
**Related:** autoplans/sub-session-handoff.md (separate plan, already committed at 3b5d5de)

## Problem

Every LLM call in the app does two phases:

1. **Prompt processing (PP)** — the model ingests the prompt (KV cache). llama.cpp
   reports live progress via `prompt_progress` stream chunks (requested through
   `return_progress: true`).
2. **Token generation (TG)** — the model streams answer tokens.

Today the UI status bar (`ui/status.rs`) only reflects the **selected session's
streaming round**:

- `AppStatus.prompt_tps` / `StreamPromptProgress` — PP speed for the visible session
  only (`handle_stream_prompt_progress`).
- `AppStatus.tps` — TG speed, same.
- `AppStatus.status` — a static "streaming" / "idle" string.

All the other LLM calls run silently — the status bar says "idle" while the judge,
the improvement check, memory maintenance, brief polish, or a headless run_eval
agent are hammering the GPU. The user wants:

1. **Every PP and TG event streamed to the status bar, including background calls.**
2. **A dynamic status label** — "streaming" should read "judge", "agent: X",
   "memory", "improvement", "eval: <id>", … i.e. whatever is actually running.

## Scope (finalized design — from code investigation 2026-07-19)

### 1. Core: a shared `ActivityTracker` (new module)

**Module placement (dependency invariant!):** `lib.rs` states `types` has NO
internal dependencies. So:

- `wuffagent-core/src/types/activity.rs` — pure data: `LlmActivityInfo`
  (label, session_id, phase, tokens_in, tokens_out, tps, started_at, duration)
  + `ActivityPhase { PromptProcessing { processed, total, time_ms }, Generating }`.
  Re-export from `types/mod.rs`.
- `wuffagent-core/src/activity.rs` (top-level `pub mod activity;` in lib.rs,
  update the module-graph comment) — `ActivityTracker` + `ActivityHandle`:

- `ActivityTracker { inner: Mutex<TrackerInner>, tx: mpsc::Sender<AppEvent> }`,
  `Arc`-cloned everywhere. `TrackerInner { next_id: u64, entries:
  HashMap<u64, Entry> }`; `Entry { label, session_id: Option<String>,
  phase, tokens_out, started: Instant, last_emit: Option<Instant> }`.
- **RAII:** `begin(label, session_id: Option<String>) -> ActivityHandle { id }`;
  `ActivityHandle::Drop` removes the entry (covers `?` early-returns, panics,
  cancellation) and emits one final snapshot.
- **Updates:** `pp(&handle, &PromptProgress)`, `tg(&handle, n_tokens)`,
  `finish(&handle, tokens_out)`.
- **Throttle ≈5 Hz per activity** (≥200 ms since last emit) — llama.cpp sends
  `prompt_progress` per server tick (can be >50 Hz); never forward faster.
  Terminal events (finish/error/drop) always emit immediately.
- **Emit:** `AppEvent::LlmActivity { activities: Vec<LlmActivityInfo> }` —
  snapshot of ALL active activities (one event = full state; the UI just
  replaces its list; no incremental bookkeeping). `tx.send` failures are
  ignored (UI gone).
- **Snapshot fields:** `label`, `session_id` (None = global/background),
  `phase`, `tokens_out`, `tps` (tokens/s for the current phase: PP =
  `PromptProgress::prompt_tps()`, TG = tokens since phase start / phase
  duration), `started_at: SystemTime`, `duration: Duration`.
- **`LabeledLlm`** (same module): a `LlmClient` impl wrapping
  `inner: Arc<dyn LlmClient>` + `tracker: Arc<ActivityTracker>` + `label:
  String` + `session_id: Option<String>` — instruments `complete` /
  `complete_with_usage` / `stream` with begin/tg/finish. Used by call sites
  that hold a dyn client + tracker. **The LlmClient trait is NOT modified**
  (test mocks stay untouched).

### 2. Core: event variant

`types/events.rs`: add

```rust
/// A live snapshot of every LLM activity currently in flight (foreground or
/// background), emitted by `ActivityTracker` (throttled to ~5 Hz per
/// activity; one snapshot also on every activity's finish/drop). The UI
/// replaces its whole list on receipt.
LlmActivity { activities: Vec<crate::types::LlmActivityInfo> },
```

(`AppEvent` has no `#[non_exhaustive]`-style exhaustiveness guard — but grep
for `match` on `AppEvent` in egui: the sid-extraction match at
`ui/event_handler/mod.rs:13` and the dispatch match at `:49` are exhaustive —
both get a new arm.)

### 3. Core: instrument every LLM call site (checklist — the "every" part)

Design rule: **the `ChatClientAdapter` itself does NOT instrument** (it stays
a dumb pass-through); instead it gains an inherent factory

```rust
impl ChatClientAdapter {
    pub fn labeled(&self, label: &str, session_id: Option<String>)
        -> Option<Arc<LabeledLlm>>; // None when no tracker was attached
}
// adapter gains `with_activity(tracker) -> Self` (bootstrap sets it)
```

and each call site below opts in with an explicit label. This makes the
coverage explicit and avoids double-instrumentation:

| # | Call site (verified by search) | Instrumentation | Label |
|---|--------------------------------|-----------------|-------|
| 1 | `agents/agent/loop.rs:394` — main round `stream_with_messages_arc` (the ONLY place PP is forwarded to the UI today) | Agent's tracker: `begin` before the call; the round callback already receives `String \| PromptProgress` → feed `tracker.pp` / `tracker.tg`; `finish` on completion | `agent: {config.name}` (overridable, see run_eval) |
| 2 | `agents/agent/verify.rs:214` — judge `self.llm_client.stream(&verification_messages, …)` | Switch to the per-session `self.client.stream(...)` + Agent's tracker (verify.rs holds `Arc<dyn LlmClient>`; using the session client + tracker avoids a trait change and double-counting), `session_id = self.session_id()` | `judge` |
| 3 | `agents/agent/loop.rs:726` — `polish_brief` → `self.client.complete_messages` (flag-gated, non-streaming → TG only) | Agent's tracker, begin/finish around the call | `brief polish` |
| 4 | memory manager LLM calls (bootstrap.rs:208 hands the adapter to `memory_manager`; maintenance pass calls `complete`) | `llm_client.labeled("memory", None)` at the call site | `memory` |
| 5 | `agents/improvement/suggest.rs:249` — on-demand check `llm_client.complete_with_usage` (tool holds the concrete adapter) | `llm_client.labeled("improvement", None)` | `improvement` |
| 6 | `agents/improvement/fleet.rs:285` — fleet check via the engine's `self.llm_client: Arc<dyn LlmClient>` (no concrete adapter there) | `LabeledLlm::new(self.llm_client.clone(), engine.activity, "improvement", None)` — the engine gains a `with_activity(tracker)` builder field (mirrors `with_memory`) | `improvement` |
| 7 | `tools/builtin/improvement.rs:1009` — `run_eval` headless `AgentBuilder::new(cfg, llm_client, client)` | `AgentBuilder` gains `.activity(tracker)` + `.activity_label(String)`; the run_eval tool builds `.activity(activity).activity_label(format!("eval: {}", eval_id))` — the eval agent's main round, judge (#2) and brief polish (#3) all pick the label up through the Agent | `eval: <id>` / `judge` / `brief polish` |
| 8 | engine-built agents (`agents/engine.rs` — find the `Agent::builder(...)` site) | engine passes its tracker + `format!("agent: {}", profile_name)` down | `agent: X` |

Grep for remaining `llm_client.complete` / `.stream(` / `complete_messages`
call sites as a final sweep in step 1's verification.

**Agent plumbing:** `Agent` struct gains `activity:
Option<Arc<ActivityTracker>>` + `activity_label: Option<String>`
(`AgentBuilder` setters; default label = `config.name`).

**bootstrap wiring:** after the `event_tx` setup (~line 175):
`let activity = Arc::new(ActivityTracker::new(event_tx.clone()));` →
`llm_client` / `memory_llm_client` `.with_activity(activity.clone())`,
engine `.with_activity(activity.clone())`, and a new
`register_improvement_tools(..., activity.clone(), ...)` parameter (for
run_eval).

### 4. UI: status bar

`wuffagent-egui/src/ui/state/groups.rs` — `DisplayState` (the "status bar
value" group) gains:

```rust
/// Live LLM-activity snapshot from the core `ActivityTracker`
/// (`AppEvent::LlmActivity`); replaced wholesale on every event.
pub llm_activities: Vec<wuffagent_core::types::LlmActivityInfo>,
```

`wuffagent-egui/src/ui/event_handler/mod.rs`:

- sid-extraction match (~line 13): `AppEvent::LlmActivity { .. } =>
  String::new()` (not session-bound).
- dispatch match (~line 49): `AppEvent::LlmActivity { activities } =>
  self.display.llm_activities = activities,` (or a small handler fn).

`wuffagent-egui/src/ui/status.rs` — `draw_status_bar(ctx, status,
session_name, llm_activities, selected_session_id)`:

- **Dynamic label (left slot):**
  - current-session activity (matching `selected_session_id`, where selected
    = `sessions.active_tab.or(selected_session_id)`) exists:
    - PP phase → `"processing prompt… <pct>%"` (pct = `processed/total`).
    - TG phase → `"streaming"`.
  - else if any background activity: `"<label> · <phase>"`
    (e.g. `"judge · streaming"`, `"agent: coder · processing prompt… 42%"`,
    `"eval: t1 · streaming"`, `"memory · thinking"`).
  - else → `"idle"`.
- **Background pills (after session name):** one small pill per non-selected
  activity: `label · tps` (e.g. `judge · 24 t/s`); cap at 2–3 pills +
  `+N`; hover tooltip = full snapshot text (label, session, phase, tokens,
  duration).
- The existing `status.prompt_tps` / `status.tps` displays stay as-is (they
  are fed by the session's own `StreamPromptProgress` / `StreamRoundComplete`
  events — no change there); the activity snapshot drives only the label +
  pills.

### 5. Out of scope

- No change to `AppStatus` or the `StreamPromptProgress` / `StreamRoundComplete`
  event flow (the session's own PP/TG numbers keep working exactly as today).
- No per-activity cost/token totals in the bar (that's the usage panel).
- No persistence of activity snapshots (transient by design).
- The sub-session handoff fix is a separate plan (already committed: 3b5d5de).

## Phases

1. **Core activity module + event** — `types/activity.rs` (LlmActivityInfo,
   ActivityPhase), `activity.rs` (ActivityTracker, ActivityHandle RAII,
   ~5 Hz throttle, LabeledLlm), `AppEvent::LlmActivity`, `ChatClientAdapter::
   labeled` / `with_activity`. Unit tests: RAII drop on `?`-return, throttle
   (fake clock or relaxed bound), snapshot contains all concurrent entries,
   LabeledLlm instruments a mock inner client.
   **Verify:** `cargo test -p wuffagent-core activity`.
2. **Instrument core call sites** — Agent plumbing (builder fields +
   activity_label), engine `with_activity` + thread into built agents,
   loop.rs main round + polish_brief, verify.rs judge (switch to session
   client), memory/improvement `labeled(...)` call sites, run_eval
   (register_improvement_tools param). Final sweep grep for un-instrumented
   LLM call sites.
   **Verify:** `cargo test -p wuffagent-core`.
3. **UI: event + state + status bar** — DisplayState field, event_handler
   arms (both matches), status.rs signature + dynamic label + background
   pills + tooltip.
   **Verify:** `cargo build -p wuffagent-egui`.
4. **bootstrap wiring** — create tracker, hand to adapters/engine/tools.
   **Verify:** `cargo build` (both crates).
5. **Manual test + commit.**

## Test plan

**Unit (core):**
- RAII: `begin` then drop the handle without `finish` → entry removed, final
  snapshot emitted.
- Throttle: N rapid `pp` calls → ≤ ~N/200ms + 1 terminal events sent (use a
  fresh channel and drain with a small wait).
- Concurrency: two entries → snapshot has both, with independent phases/tps.
- `LabeledLlm`: mock inner `LlmClient` → `complete` produces begin + finish,
  snapshot carries the right label + session_id.

**Integration / manual:**
- Start a chat turn with a big prompt → status bar shows
  `"processing prompt… 37%"` then `"streaming"`.
- Trigger `run_self_improvement` (or an improvement check) while idle →
  status bar label flips to `improvement · …` (background pill if a session
  is also streaming).
- Trigger a judge round (send a message that makes the agent verify) → label
  `judge`.
- Trigger memory maintenance → `memory`.
- Run evals from the agent editor → `eval: <id>`.
- Two concurrent: chat turn + improvement check → current-session label wins
  on the left; the other appears as a pill.

## Risks / notes

- **Event volume:** `prompt_progress` can arrive per server tick. The
  per-activity 200 ms throttle + whole-snapshot replacement keeps the UI
  load bounded (~5 events/s per active LLM call, each a small Vec). The mpsc
  relay already batches per frame (`process_pending_events`).
- **RAII + `?`:** `ActivityHandle::Drop` is the only reliable cleanup (the
  verify/judge paths use `?` and `map_err`). No manual `end()` calls needed;
  `finish()` only marks the terminal snapshot's token count.
- **Borrow-checker in the loop callback:** the stream callback already moves
  clones of `event_tx` / `session_id` / `usage_recorder` (loop.rs:401-404);
  the tracker handle + a cloned `Arc<ActivityTracker>` join them the same
  way.
- **No `types`-internal dependency:** LlmActivityInfo lives in `types`
  (data only); the tracker lives in the top-level `activity` module and may
  depend on `types`.
- **Dyn vs concrete clients:** call sites holding `Arc<dyn LlmClient>`
  (verify.rs, fleet.rs) instrument via the session client / `LabeledLlm::new`
  directly; sites holding the concrete adapter use `labeled()`. Never both —
  that's how double-counting sneaks in (the adapter itself is NOT
  instrumenting).
- **`session_id` on background calls:** None for global calls (memory,
  improvement); Some(eval session id) for run_eval agents — the UI shows
  unknown session ids as background pills regardless.

## Next step

Implement phase 1 (core `activity` module + `AppEvent::LlmActivity` +
`ChatClientAdapter::labeled`/`with_activity`), then `cargo test -p
wuffagent-core activity`, commit, continue with phase 2.
