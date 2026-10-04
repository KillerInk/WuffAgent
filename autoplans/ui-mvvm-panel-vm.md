# UI MVVM: lightweight "panel view model" split for wuffagent-egui

**Status:** OPEN (plan written 2026-10-07, wuffagent)
**Crate:** `wuffagent-egui` only — `wuffagent_core` stays untouched (no API changes).

## Goal

Bring the egui UI to a consistent, *lightweight* MVVM shape that fits
immediate mode — **without** a binding/observer framework:

- **Model** = `wuffagent_core` (already UI-free).
- **ViewModel** = per-panel state structs (private state + `update`/`mark_*`
  methods + an `Action` enum) and the existing `AppEvent` relay
  (`ChatApp.relay` + `ui/event_handler`).
- **View** = thin, mostly-pure draw functions that read panel state and
  **return** `Action`s instead of mutating `ChatApp` directly.

The codebase already has this pattern in 8 places (`UsagePanel`,
`Dashboard`, `SessionsPanel`, `SettingsDialog`, `PresetsDialog`,
`McpPanel`, `MemoryPanel`, `agent_config` — all self-owned structs with
`new` + `draw(ctx, deps)`). The work is to extend it to the remaining
`impl ChatApp` draw code and shrink the `ChatApp` god-state.

### Non-goals (decisions)

1. **No binding/notification machinery** — egui rebuilds every frame;
   `ctx.request_repaint()` + the existing `AppEvent` mpsc relay is the
   immediate-mode equivalent of property-change notification.
2. **No per-widget view models**, no `INotifyPropertyChanged`-style
   traits, no external "egui MVVM" crate.
3. **No visual/behavior change** — pure refactor; every phase is a 1:1
   move of existing logic (same widgets, same order, same strings).
4. **No `wuffagent_core` changes** — if a phase discovers core logic that
   belongs in core (e.g. a summary formatter), it is extracted inside
   `wuffagent-egui` as a pure fn with unit tests; moving it to core is a
   separate follow-up.
5. `event_handler/` staying as `impl ChatApp` during migration is
   acceptable (it is the ViewModel's *update* side, not the View); it is
   split last (Phase 5).

## Target convention (Phase 0 defines this in `ui/mod.rs` module doc)

```rust
// ui/panel_vm.rs (or documented convention in ui/mod.rs)
//
// Panel view model:
//   pub struct XxxPanel { /* private state */ }
//   impl XxxPanel {
//       pub fn new() -> Self;
//       pub fn handle_event(&mut self, ev: &AppEvent) -> Vec<XxxAction>; // core → panel
//       pub fn draw(&mut self, ui: &mut egui::Ui, shared: &Shared) -> Vec<XxxAction>;
//   }
//   pub enum XxxAction { /* user intent, e.g. Send { text }, DeleteMessage { id } */ }
//
// One dispatcher in the orchestrator (window.rs) maps actions → ChatApp
// state changes. Views never reach into ChatApp fields directly.
```

Rules:
- Panel state leaves `ChatApp` and moves into the panel struct.
- Draw fns take the narrowest possible deps (e.g. `&ChatViewState`
  snapshot struct or concrete handles), never `&ChatApp`/`&mut ChatApp`.
- `Action`s are `#[derive(Debug)]`, unit-tested where they carry logic.
- Shared read-only data that several panels need becomes a small
  `Shared`/snapshot struct built once per frame by the orchestrator
  (keeps draw fns pure-ish and testable).

## Current state (measured 2026-10-07)

Already panel-VM (reference examples, no work):
`usage_panel.rs`, `dashboard.rs`, `sessions_panel.rs`, `settings.rs`,
`presets_dialog.rs`, `mcp_panel/`, `memory_panel/`, `agent_config/`.

Still `impl ChatApp` (the work):
| file | size | notes |
|---|---|---|
| `ui/chat_area/mod.rs` | 30 KB | `draw_chat_area(&mut self, ui)` |
| `ui/chat_area/bubbles.rs` | 31 KB | message/tool-bubble rendering |
| `ui/chat_area/tool_cards.rs` | 32 KB | `draw_tool_card`, expand/collapse, delete |
| `ui/chat_area/tool_json.rs` | 16 KB | tool JSON rendering |
| `ui/input/mod.rs` | 32 KB | `draw_input_area(&mut self, ui)` |
| `ui/input/agent_profile.rs` | 8 KB | agent picker row |
| `ui/input/images.rs` | 7 KB | attach/paste images |
| `ui/status.rs` | 22 KB | `draw_status_bar(&self, ui)` + status-bar menus |
| `ui/window.rs` | 27 KB | main layout + dialog orchestration (eframe App) |
| `ui/layout.rs` | 10 KB | toolbar/panel toggles |
| `ui/host_bridge_cmd.rs` | 7 KB | plugin host-command dispatch |
| `ui/event_handler/{mod,stream,tool}.rs` | ~36 KB | AppEvent → state (update side) |
| `ui/state.rs` + `state/groups.rs` | 17 KB + 25 KB | `ChatApp` god-state (target: shrink) |

## Phases (each = one or more verified, committed turns)

**Gate for EVERY phase (no exceptions):**
1. `cargo build` (app must be closed — running exe locks the binary).
2. `cargo test -p wuffagent-egui` (plus `cargo test -p wuffagent-core`
   if anything touches the crate boundary — it shouldn't).
3. Manual smoke: open app, exercise the touched panel (send a message,
   toggle the panel, right-click a tool card, open settings) — UI is
   human-visible, so a user round-trip is required; report as
   PENDING VERIFICATION if no user is available.
4. `git commit` the phase before starting the next.

### Phase 0 — Convention + test harness (~1 turn)
1. Write the convention doc in `ui/mod.rs` (module doc + short
   `ui/panel_vm.rs` with the `Action` dispatch helper if a shared one is
   wanted — keep it small, a doc comment is enough to start).
2. Add an `egui`-free unit-test pattern: every panel gets a `#[cfg(test)]`
   module testing its non-draw logic (state transitions, `Action`
   construction, pure formatters). Example: extract
   `tool_result_summary`-style helpers from `chat_area/tool_cards.rs` and
   test them.
3. Commit. Gate: build + tests (no new behavior, so smoke = launch only).

### Phase 1 — Pilot: `status.rs` → `StatusBar` VM (~1–2 turns)
Smallest, mostly read-only → lowest risk, proves the convention.
1. New `ui/status_bar.rs` (or rename `status.rs`): `struct StatusBar`
   with private state (e.g. any menu/tooltip state currently in
   `ChatApp`), `draw(&mut self, ui, view: &StatusBarView) -> Vec<StatusAction>`.
   `StatusBarView` = snapshot struct (chat status, llm activities,
   displayed sid, server status) built per frame in the orchestrator.
2. `StatusAction`s for whatever the status bar can *do* (menus, copies,
   toggles) — dispatch in `window.rs`.
3. Remove the corresponding fields from `state/groups.rs`.
4. Commit. Gate: full (status bar is on every screen — user round-trip).

### Phase 2 — `input/` → `InputArea` VM (~2–3 turns)
1. `struct InputArea` owning: input buffer staging, pending images,
   agent-picker state, any typing state currently in
   `ChatApp.sessions.*`/`dialogs.*` that belongs to the input.
2. `InputAction`s: `Send { text, images }`, `Cancel`, `AttachImage`,
   `PickAgent`, … — the send path (which today mutates several
   `ChatApp` fields and spawns the run) is dispatched in `window.rs`,
   keeping the spawn logic where it is (just moved behind the action).
3. `draw(&mut self, ui, shared) -> Vec<InputAction>`; `input/{mod,
   agent_profile,images}.rs` become its submodules.
4. Commit. Gate: full (send a message + paste/attach an image — user
   round-trip required).

### Phase 3 — `chat_area/` → `ChatArea` VM (~3–5 turns, largest)
1. `struct ChatArea` owning: `expanded_messages`, scroll/anchor state,
   anything else per-session view state that is really *view* state.
   (Per-session chat runtime stays in `wuffagent_core::sessions::
   SessionRuntime` — only pure-view fields move.)
2. `ChatAction`s: `ToggleToolCard { msg_id }`, `DeleteMessage { msg_id }`,
   `CopyContent { … }`, scroll intents, …
3. `bubbles.rs` / `tool_cards.rs` / `tool_json.rs` become render helpers
   taking `&ChatArea` + the message snapshot — no `ChatApp` in sight.
4. This phase also lands the `Shared`/snapshot struct if Phase 1/2 made
   it obviously needed (build once per frame in `window.rs`).
5. Commit. Gate: full (multi-turn chat with tool cards, expand/collapse,
   right-click delete, sub-session tabs — user round-trip required).

### Phase 4 — `window.rs` + `layout.rs` + `host_bridge_cmd.rs` thin down (~2 turns)
1. `window.rs` ends as a pure **orchestrator**: build the per-frame
   snapshot, call panel `draw`s, dispatch the action vecs, keep dialog
   open/close plumbing (settings/presets already panel-VM).
2. `layout.rs` toolbar → `LayoutBar` VM with `LayoutAction`s (panel
   toggles, session actions) or folded into the orchestrator if that is
   smaller — decide at implementation time, prefer the smaller diff.
3. `host_bridge_cmd.rs` stays an `impl ChatApp` *command* handler (it is
   update-side, not view) unless Phase 5 pulls it in.
4. Commit. Gate: full (toolbar + all panels + settings/presets dialogs —
   user round-trip required).

### Phase 5 — `event_handler/` split + `ChatApp` shrink (~2–3 turns)
1. Move per-panel event handling into panel VMs:
   `ChatApp::handle_event` becomes a router that forwards each `AppEvent`
   to the owning panel's `handle_event` and dispatches the returned
   actions (same order, same side effects).
2. `state/groups.rs` shrinks to truly app-wide state: `core` services,
   `relay`, `restart`, and whatever panels still share. Document the
   ownership table (which panel owns which field) in `state.rs`.
3. Commit. Gate: full (run a full agent turn while watching the UI:
   stream chunks, tool cards, status bar, usage-panel dirty flag —
   user round-trip required).

### Phase 6 — Sweep + docs (~1 turn)
1. Grep `wuffagent-egui/src/ui` for remaining `impl ChatApp` — expect
   only: `state.rs` (construction), `event_handler` (router),
   `host_bridge_cmd` (commands). Anything else = leftover to fold in.
2. Update `AGENTS.md` (or `wuffagent-egui/README` if it exists) with the
   panel-VM convention + ownership table.
3. Save a memory entry recording the convention (tag
   `agent:wuffagent`) and mark this plan DONE.
4. Commit. Gate: build + tests + launch smoke.

## Risks / watch-outs

- **Borrows**: `&mut ChatApp` today gives draw fns blanket access; moving
  to narrow deps will surface borrow conflicts (the code already fights
  them — see the `sid` extraction trick in `event_handler/mod.rs`).
  Mitigation: build the per-frame snapshot struct up front so draw fns
  only read it.
- **egui widget memory**: per-frame state that egui itself stores
  (`Id`-keyed) must NOT be duplicated into panel structs — move only
  state that is used across frames outside egui's memory.
- **`apply_diff` is all-or-nothing per call** — keep SEARCH blocks small
  and unique; after additive enum diffs, grep for the variant names.
- **Build while app runs fails** (os error 5) — close the app before
  `cargo build`.
- **Drive (M:) flakiness**: commit every phase immediately; re-check with
  shell if a just-written file "disappears".
- **Visual drift**: same widget order/strings per phase; any intended
  pixel change gets its own commit + user confirmation.

## Est. effort

~10–15 verified turns total. Phases 1 and 2 are independent and can be
swapped; Phase 3 is the long pole.
