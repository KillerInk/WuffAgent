# Sub-session handoff: wire the UI handlers (steps 16 & 17)

## Status: IMPLEMENTED (commit `ea61eb5`, 2026-07)
Both arms wired in `wuffagent-egui/src/ui/event_handler/mod.rs`; resolvers widened
`pub(super)` -> `pub(crate)` in `ui/input/agent_profile.rs`. Build green (workspace),
tests green (core lib 835 + integration suites + egui 51, all `ok`, exit 0), clippy
clean on touched files, rustfmt clean on touched files. Minor deviations from the plan
below: sub-session name is `Sub: {agent}` (not a task-truncated name), and the
parent-existence guard was dropped (the event always follows the parent's own
terminal StreamComplete, so the parent is guaranteed present). PENDING: live UI
verification of a real `handoff(sub_session: true)` round-trip (incl. `hand_back`).

## Symptom
`handoff(agent, task, sub_session: true)` returns "Handoff queued successfully", the
parent session shows the `[Handed off to 'X' (sub-session)] ...` marker — and then
**nothing happens**: no sub-session tab, no sub-agent run, no new session file.
(`hand_back` from a sub-session is equally dead for the same reason.)

## Root cause (verified 2026-07)
The entire CORE chain works:
1. `handoff` tool reads `sub_session` and pushes it into the control mailbox
   (`wuffagent-core/src/tools/builtin/handoff.rs:179-185` schema, `:209` read,
   `:220-225` `ControlRequest::Handoff(HandoffRequest { sub_session, .. })`).
2. The agent loop ends the turn and emits
   `AppEvent::SubSessionHandoff { parent_session_id, agent, task }`
   (`wuffagent-core/src/agents/agent/execute.rs:191-233`), after a terminal
   `StreamComplete` for the parent.
3. The UI `handle_event` even routes the event by `parent_session_id`
   (`wuffagent-egui/src/ui/event_handler/mod.rs:36-38`).

But the UI event arms are **placeholders** — `wuffagent-egui/src/ui/event_handler/mod.rs:125-152`:
- `SubSessionHandoff` → logs "handler pending, step 16", drops the event.
- `AgentHandBack` → logs "handler pending, step 17", drops the event.

So nothing ever creates the sub-session: no `create_session`, no `SessionRuntime`,
no `sub_session_tabs.push`, no `start_pipeline_for_session`. The tab bar
(`ui/chat_area/mod.rs:30-32`, `draw_sub_session_tabs` `:256+`) and the
`sub_session_tabs`/`active_tab` state (`ui/state/groups.rs:59-68`) are all built
but never fed.

## Fix
All changes in `wuffagent-egui/src/ui/event_handler/mod.rs` (+ a 2-line visibility
widen in `ui/input/agent_profile.rs`).

### 1. `SubSessionHandoff` arm (replace placeholder, ~35 lines)
```
1. Parent guard: `self.sessions.session_store.get(&parent_session_id)`
   None → tracing::warn! + do nothing.
2. Create the sub-session file (mirror PanelAction::Create,
   ui/sessions_actions.rs:38-81):
   let sessions_dir = self.core.config.sessions_dir.clone();
   let name = format!("sub: {agent} — {}", task-truncated-to-~40-chars);
   let session = wuffagent_core::sessions::create_session(&sessions_dir, &name);
3. Build the runtime:
   SessionRuntime::create_from_config(&self.core.config, &self.core.connection,
       &self.core.agent_engine, session.id, session.name,
       self.relay.pending_tx.as_ref().unwrap().lock().unwrap().clone())
   runtime.selected_agent = Some(agent.clone());
   runtime.client.set_session_meta(SessionMeta {
       selected_agent: Some(agent),
       reasoning_mode: runtime.reasoning_mode,
       parent_session_id: Some(parent_session_id),   // ← this link is what
   });                                               //   advertises `hand_back`
                                                    //   in the sub (execute.rs:240-272)
4. Wire UI state:
   session_store.insert(id, runtime);
   selected_session_id = Some(parent_session_id);  // tab bar context stays parent
   sub_session_tabs.push(id) (skip if already present);
   active_tab = Some(id);                          // focus the new tab
   if let Some(panel) = self.sessions_panel.as_mut() { panel.refresh(); }
5. Start the first turn:
   self.start_pipeline_for_session(&id, &task, None,
       self.resolve_agent_prompt(&agent), self.resolve_tool_policy(&agent), false);
   // already_displayed = false → the task shows as a user message in the sub tab
   // (start_pipeline_for_session: ui/input/mod.rs:482; prompt/policy resolution
   //  mirrors send_message_to_session, input/mod.rs:476-478)
```
Visibility widen: `resolve_agent_prompt` / `resolve_tool_policy` are `pub(super)`
in `ui/input/agent_profile.rs:58` and `:85` — make them `pub(crate)` so the event
handler can call them.

### 2. `AgentHandBack` arm (replace placeholder, ~20 lines)
```
1. Parent guard: store.get(&to_session_id) None → warn + return.
2. Focus the parent:
   selected_session_id = Some(to_session_id);
   active_tab = None;            // back to the main (parent) tab; the sub tab
                                 // stays open for re-reading
3. Post the hand-back turn — same pattern as the UserMessageDrained arm
   (mod.rs:226-256):
   if parent is_generating → push a QueuedMessage onto the parent's queue
   else start_pipeline_for_session(&to_session_id, &task, None, prompt, policy, false)
   with prompt/policy resolved from the PARENT's selected_agent
   (pattern: input/mod.rs:430-435).
```

### 3. Edge cases
- Parent session deleted before the event → guard 1 covers both arms.
- Multiple sub-tabs from the same parent (successive handoffs): allowed; each is
  a fresh session + tab (order = creation order).
- `active_tab` reset on session switch (documented in groups.rs:66-68) already
  exists — verify the sidebar switch code actually resets it while implementing.
- The sub-session file persists with `parent_session_id` set (SessionMeta,
  core sessions/model.rs:27-39) so a reload keeps the hand_back link.

## Verification
1. `cargo build` — zero errors (both crates).
2. `cargo test -p wuffagent-egui` (unit-test the handler only if cheap; the live
   E2E is the real proof).
3. Live E2E:
   - agent calls `handoff(sub_session: true)` → sub tab appears AND is focused;
     task visible as user message; sub agent (its own profile) streams in a fresh
     session; parent shows the marker and is idle.
   - in the sub session the `hand_back` tool exists; calling it focuses the parent
     tab and runs the hand-back task as a new parent turn.
4. Re-dispatch the status-bar implementation sub-task
   (`autoplans/statusbar-llm-activity.md`) via `handoff(sub_session: true)` —
   doubles as the live verification of this fix.

## Implementation notes
- Do NOT dispatch the implementation of THIS fix via a sub-session handoff (that
  is the broken feature) — in-turn handoff or in-session work.
- PowerShell shell: chain commands with `;`, not `&&`.
