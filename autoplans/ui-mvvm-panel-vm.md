# UI split: surgical panel-state extraction for wuffagent-egui

**Status:** OPEN (v2 rewrite 2026-10-07, wuffagent — replaces the over-scoped
"full MVVM" v1; same file, same commit lineage)
**Crate:** `wuffagent-egui` only — `wuffagent_core` stays untouched.

## Goal

De-tangle the two genuinely-tangled areas of the UI by moving *view state*
out of the `ChatApp` god-struct into self-owned panel structs, and pull
pure helpers out of draw code into tested units. That's it.

The codebase already converged on the right immediate-mode shape in 8
places (`UsagePanel`, `Dashboard`, `SessionsPanel`, `SettingsDialog`,
`PresetsDialog`, `McpPanel`, `MemoryPanel`, `agent_config` — self-owned
structs with `new` + `draw(ctx, narrow deps)`). The remaining work is a
*migration of inline panels* (embedded in shared columns, cross-panel
effects) — a different class than those floating dialogs, so it is done
carefully per panel, not as a framework rollout.

### Non-goals (hard rules)

1. **No framework**: no `Action` enums, no dispatcher, no per-frame
   snapshot/`Shared` struct, no binding/notification layer. A mutation in
   a draw fn is visible and local — keep it that way.
2. **No per-widget view models**, no external "egui MVVM" crate.
3. **No visual/behavior change** — 1:1 move of existing logic (same
   widgets, order, strings). Any intended pixel change = own commit +
   user confirmation.
4. **No `wuffagent_core` changes.**
5. **Borrows**: draw fns take the narrowest borrows they need from
   `ChatApp` (several `&self`/`&mut self` slices) — the borrow checker
   handles it; do NOT introduce copy layers to dodge conflicts.
6. Move only state that is used across frames OUTSIDE egui's own
   Id-keyed widget memory — never duplicate what egui already stores.

## Current state (measured 2026-10-07)

Tangled (the work): `chat_area/{mod,bubbles,tool_cards,tool_json}.rs`
(~108 KB, `impl ChatApp`, mutates god-state from deep in draw code —
`expanded_messages`, per-session view state) and `input/{mod,
agent_profile,images}.rs` (~47 KB, input staging, pending images,
agent-picker state).

Untangled already (no work, reference examples): `usage_panel.rs`,
`dashboard.rs`, `sessions_panel.rs`, `settings.rs`, `presets_dialog.rs`,
`mcp_panel/`, `memory_panel/`, `agent_config/`.

Acceptable as-is (opportunistic only, see Follow-ups): `status.rs`
(22 KB, read-only draw + menus), `window.rs` (27 KB orchestrator),
`layout.rs`, `host_bridge_cmd.rs`, `event_handler/` (update side —
`impl ChatApp` there is correct, not a smell).

## Gate for EVERY step (no exceptions)

1. `cargo build` (app must be CLOSED — the running exe locks the binary,
   os error 5 otherwise).
2. `cargo test -p wuffagent-egui` (and `-p wuffagent-core` only if the
   boundary was touched — it shouldn't be).
3. Manual smoke: launch, exercise the touched panel (user round-trip —
   report PENDING VERIFICATION if no user available; never self-declare
   UI-verified).
4. `git commit` before the next step.

## Steps

### Step 1 — `chat_area/` → `ChatArea` struct (~2–4 turns, the long pole)

1. New `struct ChatArea` in `chat_area/mod.rs` owning the view-state
   fields that are really *view* state (e.g. `expanded_messages`,
   scroll/anchor state, any per-session display cache that belongs to the
   chat view). Per-session chat RUNTIME stays in
   `wuffagent_core::sessions::SessionRuntime` — only view fields move.
2. `ChatApp` gains `pub chat_area: ChatArea` (in `state/groups.rs`,
   replacing the scattered fields); `ChatApp::new` builds it.
3. `draw_chat_area(&mut self, ui)` and its helpers become methods/free
   fns taking `&mut ChatArea` (+ the narrow `ChatApp` borrows they need
   for data access) instead of `&mut ChatApp` wholesale.
4. `bubbles.rs` / `tool_cards.rs` / `tool_json.rs` follow: signatures
   take `&ChatArea` / the message snapshot, no `ChatApp` in sight.
5. Where pure helpers surface (summaries, formatters — e.g.
   `tool_result_summary`), extract them as free fns with `#[cfg(test)]`
   unit tests. No helper moves without its test.
6. Commit (may be 2 commits: field move / signature rewire).
   Gate: full — multi-turn chat with tool cards, expand/collapse,
   right-click delete, sub-session tabs.

### Step 2 — `input/` → `InputArea` struct (~1–2 turns)

1. `struct InputArea` owning: input staging, pending images,
   agent-picker state — the input's fields currently scattered across
   `ChatApp.sessions.*` / `dialogs.*`.
2. `ChatApp` gains `pub input_area: InputArea`; the send path keeps
   working from `window.rs`/`layout.rs` exactly as today (it reads
   `input_area` staging, spawns the run, clears the staging — same
   order, same side effects, just through the new field).
3. `draw_input_area(&mut self, ui)` → takes `&mut InputArea` + narrow
   borrows; `agent_profile.rs` / `images.rs` follow.
4. Extract pure helpers + tests as in Step 1.
5. Commit. Gate: full — send a message, paste/attach an image, switch
   the agent picker.

### Step 3 — `status.rs` → narrow signatures (~1 turn, do or skip)

Only if Steps 1–2 surface real friction here. Options, smallest first:
(a) leave it alone (it's read-only draw + menus — acceptable as-is);
(b) convert `impl ChatApp` methods to free fns
   `fn draw_status_bar(ui, chat: &ChatViewState-ish borrow, activities,
   server_status, theme)` — no struct, no state (there is none worth
   owning). Decide at implementation time; (a) is a valid outcome.
Gate: build + tests + launch (status bar is on every screen).

### Step 4 — Sweep + docs (~1 turn)

1. Grep `wuffagent-egui/src/ui` for `impl ChatApp` — expected survivors:
   `state.rs` (construction), `event_handler/` (event routing),
   `host_bridge_cmd.rs` (commands), `window.rs`/`layout.rs`
   (orchestration). Anything new in `chat_area/`/`input/` = leftover to
   fold.
2. Short module doc in `ui/mod.rs` (or `AGENTS.md`): the 4-line
   convention — *panel = self-owned struct for its view state; draw fns
   take narrow borrows; mutations stay in the draw fn; no action
   enums/snapshots* — plus which struct owns which state.
3. Save a memory entry recording the convention (tag `agent:wuffagent`),
   mark this plan DONE.
4. Commit. Gate: build + tests + launch smoke.

## Follow-ups (opportunistic, NOT part of this plan)

- `window.rs` (27 KB) thin-down once panels are self-owned — only if it
  actually hurts navigation.
- `event_handler/` split into per-panel `handle_event` methods — only
  if event routing becomes a pain point.
- Moving extracted pure helpers into `wuffagent_core` (e.g. if a tool
  card summary becomes shared) — separate work, needs core API review.

## Risks / watch-outs

- **Borrows**: the rewire will surface borrow conflicts (the code
  already fights them — see the `sid` extraction trick in
  `event_handler/mod.rs`). Resolution: restructure borrows locally;
  do NOT add snapshot/copy layers.
- **egui widget memory**: don't duplicate `Id`-keyed state into the new
  structs (check what's actually used across frames before moving a
  field).
- **`apply_diff` is all-or-nothing per call** — small unique SEARCH
  blocks; after additive enum/struct diffs, grep for the new names.
- **Build while app runs fails** — close the app first.
- **Drive (M:) flakiness** — commit every step; re-check via shell if a
  just-written file "disappears".
- **Cross-panel effects** (inline panels affect siblings — e.g. input
  send triggers chat-area scroll): keep those in the orchestrator
  (`window.rs`/`layout.rs`) reading the panel structs, never as
  panel→panel direct references.

## Est. effort

~4–7 verified turns total (v1 estimated 10–15 for the framework version).
Step 1 is the long pole; Step 3 may legitimately end as "left as-is".

## v1 → v2 diff (why this rewrite)

Dropped: `Action` enum + dispatcher (hides local mutations for no
testable gain), per-frame `Shared`/snapshot struct (copy layer solving a
problem the borrow checker already solves), formal Phases 4–6
(window/layout thin-down, event_handler split) — all demoted to
opportunistic Follow-ups. Kept: the two real extractions (chat_area,
input), pure-helper extraction with tests, the gate discipline.

## Progress

- [x] **Step 1a** (commit `388b18d`): `tool_cards.rs` pure helpers → free fns + 17 unit tests. `parse_tool_card`, `tool_result_image_uri`, `tool_result_summary`, `data_uri_to_bytes` are now module-level; call sites rewired. `cargo test -p wuffagent-egui tool_cards` = 17 passed.
- [ ] **Step 1b**: `bubbles.rs` (`strip_thinking_tags`, `display_content_ref`) + `mod.rs` (`day_label`, `format_duration`, `tool_icon`, `char_width`, `is_at_bottom_from_output`, `breaking_label`) → free fns + tests. NOTE `format_duration`/`tool_icon` are called cross-module via `Self::` from bubbles/tool_cards/tool_json — must switch those to `super::` (or keep on ChatApp).
- [ ] **Step 1c**: rewire `draw_chat_area` + the self-using draw fns to take `&mut ChatArea` (or `&mut ChatApp` narrowed) instead of the god-object; commit.
- [ ] **Step 2**: `InputArea` extraction. **Step 3**: status.rs (may stay as-is). **Step 4**: sweep + docs.
