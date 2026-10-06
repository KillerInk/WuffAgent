# Cancel a running shell tool call (per-call Stop)

**Task:** Add an option that allows the user to cancel a shell tool call (the live "running" tool card gets a Stop button; clicking it kills the process immediately).

## Context / current state

- The whole-run Stop button aborts the run task, but `PendingToolRuns::start()`
  (`wuffagent-core/src/agents/agent/tool_exec.rs:54`) spawns each tool call with
  `tokio::spawn` and only ever `.await`s it — `tokio::spawn` is not cancellable,
  and `execute_with_progress` (manager.rs) runs the tool inside
  `tokio::task::spawn_blocking`. An aborted run task leaves the blocking tool
  task running to completion.
- `execute_tool_call` (tool_exec.rs:123) already has the right shape for the
  LLM-stream half (`tokio::select!` on the run `CancellationToken`), but there
  is no token for the tool execution half, and no per-call identity the UI
  could address.
- The live tool card (`draw_active_tool_card`, `wuffagent-egui/src/ui/chat_area/bubbles.rs:155`)
  already shows per-call state: `ActiveTool { tool_name, call_id, args_preview,
  started_at, live_output }` in `wuffagent-core/src/sessions/runtime/mod.rs:16`,
  pushed on `ToolCallStart` in `wuffagent-egui/src/ui/event_handler/tool.rs:11`.
- The shell tool (`wuffagent-core/src/tools/builtin/shell.rs`) spawns
  `std::process::Command` (powershell/bash/cmd) and pumps output through an
  mpsc channel in a loop — it only has a timeout, no kill hook.

## Design (decided)

**Per-call `CancellationToken` in a shared registry, keyed `"{session_id}:{call_id}"`.**

1. **`tools/cancel.rs` (new):** `CancelRegistry = Arc<Mutex<HashMap<String, CancellationToken>>>`
   with `register(key, parent) -> CancellationToken` (child of parent token so
   run-level `cancel()` cascades), `cancel(key) -> bool`, `deregister(key)`,
   `cancel_all()`. Exported from `tools/mod.rs`.
2. **`tools/types.rs`:** `Tool` trait gains a default method
   `fn execute_with_cancel(&self, params, progress, cancel: &CancellationToken) -> ToolResult<ToolOutput>`
   that ignores the token and delegates to `execute_with_progress`. Only the
   shell overrides it.
3. **`tools/manager.rs`:** new `execute_with_progress_and_cancel(..., cancel)`;
   `execute_with_progress` delegates with a fresh token.
4. **`agents/engine.rs` `RunParams`:** add `tool_cancel: Option<Arc<CancelRegistry>>`;
   `execute_with_tools` wires it into the agent builder.
5. **`agents/agent/mod.rs`:** `Agent.cancel_registry: Arc<CancelRegistry>`
   (builder option, default fresh registry) — same pattern as `injection_rx`.
6. **`agents/agent/tool_exec.rs`:** `PendingToolRuns` takes the registry;
   `start()` registers a child token of the run token under
   `"{sink.session_id()}:call_id"` and passes it to `execute_tool_call`, whose
   `select!` gains the tool-token arm — so a cancelled tool returns
   "Cancelled" to the loop immediately even if the process takes a moment to die.
   `clear()`/`abort_all()` cancel_all on the registry (defensive).
7. **`agents/chat_pipeline.rs`:** create a run-scoped registry in `start()`,
   store it in `RunState`, pass it via `RunParams`; new
   `pub fn cancel_tool(&self, key: &str) -> bool`.
8. **`tools/builtin/shell.rs`:** `run_command` takes a `&CancellationToken`;
   the output-pump loop checks `cancel.is_cancelled()` → kill process →
   `wait()` (reaps, no zombie) → `Err("Command was cancelled by the user
   (process stopped)")`. `execute_with_cancel` override passes the token in.
9. **UI:**
   - `ActiveTool` gains `pub cancelling: bool` (set on push to `false`).
   - `draw_active_tool_card` gains a Stop button in the header row (always
     visible, not hover-only; label "stopping…" once clicked, elapsed label
     switches to "stopping · Xs"). On click: look up the session's
     `runtime.pipeline.cancel_tool(key)`; on `true`, set `cancelling` on the
     store's `active_tools` entry and bump `active_tools_revision` so the
     snapshot redraws.
   - Button shows for every live card (generic mechanism); the shell is the
     one that actually kills — other tools return "Cancelled" to the loop
     immediately via the select arm.

**Result model:** cancelled tool produces a tool-result error
("Error: Command was cancelled by the user (process stopped)") that the loop
feeds back to the LLM so it can adapt (e.g. break the command into smaller
pieces) instead of a silent `[CANCELLED]` run abort.

## Known limitations (document, not fixed)

- Killing `powershell.exe` kills the shell process; grandchildren (e.g. a
  `cargo` launched from it) may be orphaned on Windows — same limitation as
  the existing timeout path.
- The run-level Stop still aborts the whole run (unchanged).

## Implementation order (short verified turns)

- [x] A: plan file + commit
- [x] B: `tools/cancel.rs` + exports + `types.rs` trait method + `manager.rs` → `cargo check`
- [x] C: engine RunParams + agent builder/loop/tool_exec wiring + test call-site updates → `cargo check`
- [x] D: chat_pipeline registry + `cancel_tool` + shell kill + unit tests → `cargo test -p wuffagent-core tools`
- [x] E: UI (ActiveTool field, event_handler, bubbles button, chat_area call site) → `cargo check` (exe in use: built via self-restart)
- [x] F: full `cargo test` (905 core + 95 egui, 0 failed) + commit + restart for user round-trip verification

## Verification

- Unit: `CancelRegistry` register/cancel/deregister/cancel_all/parent cascade;
  shell `execute_with_cancel` with pre-cancelled token stops a slow command
  fast (<5s for a 30s sleep) with the "cancelled" error; existing
  `cancelled_token_aborts_slow_tool_quickly` still passes.
- Round trip (user): run a long shell command, watch the live card, click
  Stop → card flips to "stopping…", process dies, tool error message appears,
  LLM continues.
