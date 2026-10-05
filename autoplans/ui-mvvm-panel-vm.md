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
- [x] **Step 1b** (commit `f919e12`): `bubbles.rs` `strip_thinking_tags` + `display_content_ref` → free fns (+2 tests); 3 `Self::breaking_label` → `super::`. `mod.rs` `char_width` + `breaking_label` now unit-tested (egui `Context` via `Context::run_ui`, `FullOutput.textures_delta.clear()`); the other 4 helpers (`day_label`/`tool_icon`/`format_duration`/`is_at_bottom_from_values`) were already tested. `tool_cards.rs`/`tool_json.rs` cross-module `Self::{tool_icon, breaking_label, format_duration, char_width}` → `super::`. `cargo test -p wuffagent-egui` = 91 passed.
- [x] **Step 1c** — rewire the chat-area draw code off the god-object:
  - [x] **1c-i: pure render fns → free fns** (commits `2d2ed54`→`7a85c42`, parts 1–4). Out of `impl ChatApp` now: `draw_avatar`, `draw_streaming_line`, `draw_active_tool_card`; `code_block`; `draw_data_uri_image`, `draw_tool_plain_result`, `draw_tool_path_badge` (tool_cards `impl ChatApp` now holds only `draw_tool_card`); `draw_tool_json_result` + `draw_tool_json_kv` (tool_json `impl ChatApp` removed entirely). All take `(ui, theme, data)` only. Call sites rewired incl. cross-module `super::tool_cards::` / `super::tool_json::`. `cargo test -p wuffagent-egui` = 91 passed at each commit.
  - [x] **1c-ii: impure self-using fns → `&mut ChatArea` + narrow borrows** (commits `ad1d3d0`→`c1c604e`). Every impure self-using fn is off `impl ChatApp`: `draw_sub_session_tabs`(ui,theme,&mut SessionState); `commit_message_edit`(sessions,&mut ChatArea,index) + `delete_message`(sessions,index) (save_session is &self); `save_message_feedback` + `draw_feedback_row`(sessions,&MemoryManager,ui,index,theme); `draw_tool_card`+`cached_tool_card`(sessions,ui,message,index,theme); `draw_message`(sessions,&MemoryManager,&mut ChatArea,ui,message,index,theme). The last 3 mod.rs fns (`draw_empty_state`/`draw_date_separator` pure; `draw_scroll_to_bottom_button` IMPURE→&mut SessionState, inlines `displayed_session_id` as `sessions.active_tab.as_deref().or(selected_session_id)`) moved out too. `impl ChatApp` in chat_area/ now holds ONLY `draw_chat_area` (the top orchestrator that extracts `&mut self.sessions` / `&self.core.memory_manager` / `&mut self.chat_area` for every helper). `cargo test -p wuffagent-egui` = 91 passed at each commit.
- [x] **Step 2** — `InputArea` extraction (commits `cc17504`→`d90cef5`):
  - **2.1/2.2 (commit `cc17504`)**: new `pub struct InputArea` in `ui/input/mod.rs` owning `pending_images` (the image staged for the NEXT message — the ONLY egui-side input view state; the staged `input_text` + selected agent are per-session RUNTIME in core's `SessionRuntime` and stay, per the no-core-changes rule). `ChatApp` gains `pub input_area: InputArea`; the field is removed from `SessionState` (groups.rs). All 6 access sites rewired to `input_area.pending_images` (input/mod.rs preview + send-clear + send-path x2 incl. the multi-line drain chain, images.rs paste/attach insert, sessions_actions.rs delete-cleanup). Send path behaviour unchanged.
  - **2.4 (commit `d90cef5`)**: `validate_input` (only `text.trim().is_empty()`, vestigial `&self`) → module-level pure free fn + 2 unit tests. The other pure input helpers (image_source_data_uri, data_uri_b64, clipboard/pixels/png fns) were already free fns pre-refactor.
  - **2.3 (deliberately left as view-model methods)**: `draw_input_area` is the draw ORCHESTRATOR (stays a method, like `draw_chat_area`); `paste_image_from_clipboard`/`attach_image_from_clipboard_or_file`/`attach_rgba` are ACTION/event handlers (correct MVVM — the view model owns events; they now write through `self.input_area`); `agent_profile.rs` (`resolve_tool_policy`/`resolve_agent_prompt`/`get_agent_names`/…) is config-driven agent RESOLUTION reading `self.core.config`, `pub(crate)` + called cross-module by the event handler — not view state or draw, so it stays. `cargo test -p wuffagent-egui` = 93 passed at each commit.
- [ ] **Step 3**: status.rs (may stay as-is). **Step 4**: sweep + docs.
