# Plan: Stream every PP/TG to the UI status bar (incl. background) + dynamic status label

Status: **PROPOSED**
Scope: wuffagent-core (new `activity` module, `llm.rs` adapter, `agents/agent/loop.rs`, `verify.rs`, events) + wuffagent-egui (`status.rs`, `event_handler`, `state/groups.rs`)

## Goal

1. **Every LLM call** — not just the selected session's streaming rounds — surfaces live
   PP (prompt-processing) and TG (token-generation) progress in the UI status bar.
   Background calls today are invisible: judge, self-improvement checks, memory
   maintenance/dedup, context-brief summarization, `run_eval` headless agents.
2. The status bar label must say **what is actually running**, not a static "Streaming":
   e.g. `judge`, `eval: greet`, `memory`, `improvement`, or the agent profile name
   during a main round.

## Current state (verified)

| Where | What happens today |
|---|---|
| `wuffagent-egui\src\ui\status.rs` (249) | Top bar: `AppStatus::Generating` → static **"Streaming"** (lines 39–45). Bottom bar: PP pill + TG pill from the **selected session's** `ChatAreaState` only (`prompt_progress`, `prompt_tps`, `gen_tps`, `live_gen_*`); hidden entirely unless `is_generating`. |
| `wuffagent-core\src\agents\agent\loop.rs` (~410–458) | Main agent round streams via `ChatClient::stream_with_messages_arc`; forwards `StreamChunk` / `StreamThinkingChunk` / `StreamPromptProgress` events with the session id. **This is the only LLM path whose PP/TG reaches the UI.** |
| `wuffagent-core\src\agents\agent\verify.rs:246` | Judge: `self.llm_client.stream(&verification_messages, Box::new(\|_chunk\| {}))` — the `LlmClient` adapter streams internally but **swallows every chunk and the PP callback** (`llm.rs` passes `\|_\| {}` as `on_prompt_progress`). Runs inside the session pipeline (`is_generating = true`) → UI shows "Streaming" with stale/empty PP/TG. |
| `wuffagent-core\src\llm.rs` | `ChatClientAdapter` wraps a `ChatClient`: `complete` / `complete_with_usage` (non-streaming) and `stream` (streaming, PP callback dropped). |
| `wuffagent-egui\src\bootstrap.rs` (281–327) | Two adapters are created: `llm_client` (shared by the agent engine → judge, brief summary, sub-agents, improvement checks, `run_eval` judge) and `memory_llm_client` (memory maintenance/dedup/auto-improve, 3600 s timeout). |
| `improvement/suggest.rs:249`, `improvement/fleet.rs:285` | Self-improvement checks: `llm_client.complete_with_usage` — **non-streaming**, no live PP/TG. Final llama.cpp `timings` ARE folded into the response `usage` (`client/http.rs:237–244`) → final PP/TG speeds are available at call end. |
| `tools/builtin/improvement/run_eval.rs` | Headless agent: `AgentBuilder::new(cfg, llm_client, fresh_client)` with **no `event_tx`** (line 108–110) → its main-round stream + judge are fully invisible to the UI. |
| `wuffagent-core\src\sessions\runtime\mod.rs` (30–96) | `ChatAreaState` per session: all live PP/TG counters live here, updated by the `event_handler\stream.rs` arms. |
| `wuffagent-core\src\types\events.rs` | `AppEvent` is `#[derive(Clone)]`, routed to the UI via the shared mpsc channel, per-`session_id`. |

**Key structural fact:** ALL non-session LLM traffic funnels through one of the two
`ChatClientAdapter` instances in `bootstrap.rs`, or through `stream_with_messages_arc`
inside an `Agent` main round (session client). That is the choke point to instrument.

## Design

### 1. Core: new `wuffagent_core::activity` module

A small registry of in-flight, **labeled** LLM activities:

```rust
pub struct LlmActivity {
    id: u64,
    label: String,                 // "judge", "eval: greet", "memory", "improvement", "summary", profile name
    session_id: Option<String>,    // Some = tied to a session pipeline (judge); None = app-level background
    phase: ActivityPhase,          // PromptProcessing | Generating (set from first PP tick / first chunk)
    prompt_progress: Option<PromptProgress>,  // live PP (llama.cpp ticks)
    gen_chars: u64,                // live TG estimate (chars, same 3.5 chars/token ratio as ChatAreaState)
    gen_started: Option<Instant>,
    last_ui_send: Option<Instant>, // throttle
}

pub struct LlmActivityInfo { /* Clone-able view of the above for the UI */ }

pub struct ActivityTracker { /* Arc<Mutex<HashMap<u64, LlmActivity>>> + Option<event_tx> + counter */ }
```

API (all cheap, in-memory only):
- `begin(label, session_id) -> ActivityHandle` — RAII; `Drop` removes the entry (covers
  errors/early returns/`tokio::time::timeout` drops).
- `prompt_progress(&handle, pp)` — sets phase `PromptProcessing` + live PP.
- `chunk(&handle, n_chars)` — sets phase `Generating`, bumps `gen_chars`.
- `finish(&handle, timings: Option<LlamaTimings>)` — stores final server speeds (for
  non-streaming calls that only report at the end), then removes the entry.
- `emit_if_due(&handle)` — throttled (≈5 Hz per activity; always on phase transition /
  begin / end) send of `AppEvent::LlmActivity { activities: Vec<LlmActivityInfo> }`.

### 2. Event

```rust
// types/events.rs (additive variant, Clone-only enum — trivial)
LlmActivity { activities: Vec<LlmActivityInfo> }   // app-level, NO session_id routing
```

The `Vec` snapshot (not per-delta events) keeps the UI state trivially consistent under
concurrent activities and bounds the channel load to `#activities × 5 Hz`.

### 3. Instrumentation points

a) **`ChatClientAdapter` (`llm.rs`)** — add optional `activity: Option<Arc<ActivityTracker>>`
   + default `label: String` (bootstrap sets `"agent"` / `"memory"`).
   - `stream`: `begin` → wrap the caller's chunk handler to also `chunk(handle, len)`;
     forward `on_prompt_progress` to the tracker (currently `\|_\| {}`) → `finish` with the
     SSE usage `timings`.
   - `complete` / `complete_with_usage` (non-streaming): `begin` → call → `finish` with the
     response `usage.timings` (already parsed, `http.rs:237–244`). No live PP/TG while
     in flight (server sends nothing) — the UI shows label + elapsed; final speeds land
     on end.
   - New cheap wrapper for per-call label overrides:
     `fn labeled(&self, label: &str) -> LabeledLlm` (holds `Arc<dyn LlmClient>` + String,
     forwards everything; `LabeledLlm: LlmClient`). `for_session(sid)` sets the
     `session_id` on `begin`.
   - `new(...)` signature stays source-compatible (tracker defaults to `None`); add
     `with_activity(tracker, label)`.

b) **Judge (`verify.rs:246`)** — `self.llm_client.labeled("judge").for_session(&self.session_id())
   .stream(...)` → live PP/TG for the judge, attributed to the session (drives the status
   label, see §4).

c) **Agent main round (`loop.rs` ~410–458)** — around the existing
   `stream_with_messages_arc` call, `tracker.begin(profile_name, Some(session_id))` and reuse
   the *existing* closures (`on_prompt_progress` and the chunk callback) to also feed the
   tracker. The UI already receives the chunk/PP events for this path — the tracker entry
   exists only to supply the **label** (and a fallback if event routing is absent).

d) **`run_eval` (`run_eval.rs`)** — set an eval-specific label on the headless agent
   (new `AgentConfig.activity_label: Option<String>`, default `None` → profile name;
   `run_eval` sets `format!("eval: {eval_id}")`, `session_id = None` → background pill).
   The loop.rs hook (c) covers its main round; the judge path (b) covers its grading call
   (label "judge" — or `eval: <id> judge` via the same `activity_label` prefix; pick the
   prefixed form during implementation).

e) **Call sites that already go through the adapters** (covered by (a) for free, with
   per-call labels via `labeled(...)` where it helps):
   - `trimming/brief.rs` summary → `labeled("summary")`
   - `improvement/suggest.rs` + `fleet.rs` → `labeled("improvement")` (non-streaming:
     label + final speeds)
   - memory maintenance / dedup / auto-improve via `memory_llm_client` → adapter default
     `"memory"` (non-streaming per batch step: label + final speeds)

f) **Bootstrap (`bootstrap.rs:297–320`)** — construct the tracker with the app's event
   sender and pass it into both adapters. Headless/tests: tracker without an event sender
   (pure no-op, zero behavior change).

### 4. UI: status bar + event handling

**State** (`state/groups.rs` → `DisplayState`): `pub llm_activities: Vec<LlmActivityInfo>`.

**Event handler** (`event_handler/`): new `LlmActivity` arm — replace the vector.
No per-session routing; it is app-level.

**Status bar (`status.rs`) — dynamic label (requirement 2).** Replace the static
"Streaming" with a computed label for the selected session:

1. an activity with `session_id == selected session` is active (the judge) →
   **`judge`** (its label), and the PP/TG pills show *that activity's* live values
   (session-state PP/TG is stale during the judge — its round already completed);
2. else `is_generating` → **`PP`** while `prompt_progress.is_some()`, else **`TG`**
   (optionally prefixed with the selected agent profile name, e.g. `coder · TG`);
3. else → `Ready` / error as today.

**Background pills (requirement 1).** In the bottom bar, after the session's own PP/TG,
draw one pill per active activity with `session_id != selected session` (i.e. background +
other sessions): `🧠 judge PP 245 t/s`, `🧠 memory …`, `🧠 eval: greet TG 12 t/s`.
Cap the row (e.g. first 3 + `+N`). Pills must render even when the selected session is
not generating (that is the whole point of this change).

## Phases (each independently shippable + committed)

1. **Core plumbing**: `activity` module + `LlmActivityInfo` + `AppEvent::LlmActivity` +
   adapter instrumentation (a) + bootstrap wiring (f) + unit tests.
   *Result: memory/improvement/brief background calls appear as pills (label + final
   speeds); judge still unlabelled.*
2. **Judge + main-round labels**: (b) + (c) + UI state/event arm + status label logic
   (item 4). *Result: "Streaming" → "judge"/"PP"/"TG"; judge PP/TG live in the pills.*
3. **run_eval + polish**: (d) + background pills row (item 4) + `AgentConfig.activity_label`.
   *Result: eval headless runs visible as `eval: <id>` pills.*

## Testing

- **Unit (core, no UI)**:
  - tracker: begin/PP/chunk/finish/snapshot; RAII drop clears; finish-with-timings stores
    final speeds; throttle sends ≤ 5 Hz per activity but always on transitions
    (assert with a mock `mpsc::Sender<AppEvent>` receiver).
  - `LabeledLlm` forwards to inner client and applies the label (mock `LlmClient`).
  - adapter `stream`: mock SSE server (reuse the `run_eval.rs::spawn_mock_sse` pattern,
    extended with a `progress` chunk) → tracker receives begin → PP → TG → finish.
- **Regression**: `cargo test -p wuffagent-core` + `-p wuffagent-egui`; existing
  verify/judge tests must pass unchanged (adapter without a tracker = today's behavior).
- **Manual**: local llama.cpp server; trigger a self-improvement check / memory
  maintenance while idle → pill appears with PP/TG; run a turn with tools → label flips
  `TG → judge → TG` per round.

## Risks / notes

- **Event flood**: bounded by the throttle (§1) — never per-token on the channel.
- **Mutex contention**: tracker lock is held for an in-memory field write per chunk;
  negligible next to the existing `Arc<Mutex<..>>` usage in the same hot path.
- **Non-streaming calls** (improvement, memory): no *live* PP/TG exists server-side —
  label + elapsed + final speeds at end. (Optional follow-up: switch those to
  internally-streamed completion like the judge, for live numbers — same pattern, not
  in this plan.)
- **Concurrent activities** (judge + memory maintenance): both pill rows coexist; the
  `Vec` snapshot keeps them consistent.
- **`AppStatus` unchanged**: the label text lives in the UI; no core enum change needed
  (keeps it Clone/Default/PartialEq stable).
- **Backends without llama.cpp `timings`/progress**: pills show label + `…` (elapsed),
  no speeds — same degradation the current PP pill already has.
