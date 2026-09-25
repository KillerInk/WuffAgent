# Finish the self-improvement loop + audit leftovers

**Status:** ✅ DONE (2026-08-31 → 2026-09-25, wuffagent). All phases A-D
complete: A (quick wins + T4 resume-failure banner) 2026-08-31, B (mcp god
file split) + C (K1 skills) + D (M1 metrics) 2026-09-25. `plans/lego.md`
is stale — its god files were already split by later refactors.

## Audit findings (2026-08-31)

- `ToolManager::validate` (tools/manager.rs:428) is a no-op **and never
  called** from `execute` — malformed self-generated calls reach the tool
  unfiltered. (T5/M2.)
- `tools/builtin/mcp.rs` is 32.8 KB with 9 tools + shared helpers in one file
  (a `mcp/tests.rs` subdir already exists). The only real god file left.
- `plans/lego.md` — superseded (see above).
- Stale memories: `fed257a1` (shell test_dangerous_command_blocked — now
  passes), `dd09a41e` (pipeline-merge dead code — ChatEngine/PipelineState/
  draw_pipeline_panel no longer exist anywhere), `7ea4c897` (gap-analysis
  status snapshot — outdated after T2/T3b/test_cmd/.prev).
- `self-improvement-gaps.md` T4 note (L462-469) says `test_cmd` still open —
  wrong, it is implemented in `tools/builtin/restart.rs` (L15, L245, L279).
  Only the **resume-failure toast** of T4 remains.

## Phase A — quick wins (~½ day) — ✅ DONE 2026-08-31

A1: `config::consume_restart_marker` (+ test override) in config/paths.rs;
main.rs consumes it, tracks `marker_session_id`, and on load failure passes
`auto_resume_failed: Option<(String, String)>` to `ChatApp::new`;
`RestartState.resume_failed` + dismissible amber top banner in
`ui/window.rs` (egui 0.36 `egui::Panel::top` + `panel_fill`; `TopBottomPanel`
and `Frame::none()` no longer exist). 3 core tests.

A2: `tools/validation.rs` (`validate_params` → Vec<String>), registry
`schema_for`, `ToolManager::validate` implemented, validation gate in
`execute_with_progress` before execution. 14 validation tests + 2 manager
integration tests. Note: "integer" accepts whole-number floats (42.0) —
rejecting `300.0` for `timeout_ms: integer` is noisier than useful;
required+null is reported once (by the required loop, not the type loop).

### A1. Restart resume-failure toast (T4 remainder)
`main.rs:366-428`: the marker is consumed (`config.session_id` set, marker
deleted) but if that session does not exist, L407-428 skips it silently —
the user gets an empty window with no explanation.
- Keep `marker_session_id: Option<String>` after marker consumption.
- After session-store init (after L428): if marker existed and
  `session_store.get(&marker_session_id).is_none()` → set a one-shot flag on
  ChatApp (e.g. `restart_resume_failed: Option<(session_id, reason)>`).
- UI: draw a dismissible banner at top of the chat window:
  "Restart resume failed — session '<id>' not found. Reason was: <reason>".
  Use the existing notification/banner pattern if one exists in ChatApp;
  otherwise a simple `Option` cleared on dismiss (no persistence needed —
  it is a one-shot startup condition).
- Testable core: extract `consume_restart_marker() -> Option<RestartMarker>`
  (read + parse + delete) into `wuffagent-core/src/config` with tests
  (marker present → Some + file gone; corrupt → None + file kept; absent →
  None).

### A2. Shallow tool-parameter validation (T5/M2)
- New module `wuffagent-core/src/tools/validation.rs`:
  `pub fn validate_params(schema: &JsonSchema, params: &ToolParams)
  -> Vec<String>` returning a list of human-readable problems (empty =
  valid):
  - every `required` field present and not `Value::Null` (unless the field
    is `nullable`)
  - declared `properties`: JSON type must match `FieldSchema.type_name`
    (string/boolean/number/integer/object/array; "integer" accepts only
    i64/u64, "number" also f64; null accepted iff `nullable`)
  - unknown keys: ignored (plugins/tools may tolerate extras)
  - arrays: check is-array only (FieldSchema has no item types)
- Wire into `ToolManager::execute` (manager.rs:400-414) BEFORE
  `spawn_blocking`: look up the tool's `JsonSchema` via a registry accessor
  (add `ToolRegistry::schema_for(&self, name) -> Option<&JsonSchema>` if not
  present) and `return Err(ToolError::InvalidParams(joined problems))` on
  failure — `InvalidParams` is `is_fixable()`, so the LLM can self-correct.
- Update the stale doc comment on `validate` (manager.rs:424-433): either
  implement it as a thin wrapper over `validation::validate_params` (so the
  public API is real) or remove it; keep one code path.
- Tests (validation.rs): missing required, wrong type per kind, integer vs
  number, nullable null, non-nullable null, unknown key tolerated, empty
  schema passes, all-good passes. Manager-level test: a registered tool with
  a schema rejects a bad param map with `InvalidParams` naming the field.

## Phase B — split the mcp god file (~½ day) — ✅ DONE 2026-09-25
`tools/builtin/mcp.rs` (32.8 KB) → module dir (converted to `mcp/mod.rs` +
submodules):
- `mcp/mod.rs`: module docs + shared helpers (`config_path`,
  `atomic_write_config`, `update_mcp_servers_in_config`, `run_mcp_op`,
  `status_str`, `mcp_err`, `notify_config_changed`, `parse_server_config`)
  + `pub use` re-exports so `register_mcp_tools` in `builtin/mod.rs` is
  unchanged. (`register_mcp_tools` itself lives in `builtin/mod.rs`.)
- `mcp/servers.rs`: mcp_add_server, mcp_remove_server, mcp_connect,
  mcp_disconnect
- `mcp/tool_mgmt.rs`: mcp_list, mcp_refresh_tools, mcp_set_tool_enabled
- Submodules pull the shared helpers via `use super::*;` (private parent
  items are visible to child modules; glob imports don't warn on unused
  names). Existing `mcp/tests.rs` stays untouched (its `use super::*;`
  still resolves the helpers from `mod.rs`).
Pure move — no behavior change. Gate passed: `cargo test -p wuffagent-core
mcp` → 24 passed, 0 failed; no warnings; workspace build EXIT=0.

## Phase C — K1: skills (procedural memory, ~1-2 days) — ✅ DONE 2026-09-25
Implemented as planned (modular: core store + tools + injection, no god class):
- `memory/skills.rs`: `SkillStore` (root `<wuffagent_home>/skills/`, test override
  `set_skills_dir_for_testing`), `Skill`/`SkillMeta`, slug-validated names
  (lowercased, a-z/0-9/'-', 1-64), atomic writes, tolerant parsing (missing
  frontmatter = body-only; unterminated frontmatter = skipped),
  `prompt_block()` (name + when_to_use + description, capped at 20) and
  `build_skills_prompt_block()` for the default store. 13 unit tests.
- `tools/builtin/skills.rs`: `save_skill` / `list_skills` / `read_skill` /
  `delete_skill` (each its own small Tool struct, `Arc<SkillStore>`-bound,
  registered via `register_skill_tools` in builtin/mod.rs + main.rs after the
  memory tools). 6 tool tests (round-trip + error paths).
- Injection in `agents/agent/prompt.rs`: `═══ SKILLS ═══` block after the
  memory block (empty string when no skills) + skill-tool guidance gated on
  `allowed_tools` (empty = all, else must contain `save_skill`).
- wuffagent profile (`~/.wuffagent/agents/wuffagent.json`): 4 skill tools
  added to `allowed_tools` so the profile can dogfood them.
Gate: `cargo test --workspace` → core 603 (+19), egui 24, 1 doctest, EXIT=0,
no warnings.
Per self-improvement-gaps Phase 4:
- Core `wuffagent-core/src/memory/skills.rs`: `SkillStore` rooted at
  `<config_dir>/skills/`. File = `---` frontmatter (name, description,
  when_to_use) + markdown body. API: `save` (atomic write, slug-validated
  name, overwrite = version via file mtime), `list() -> Vec<SkillMeta>`
  (name/description/when_to_use only), `read(name) -> Option<Skill>`,
  `delete(name)`.
- Tools `wuffagent-core/src/tools/builtin/skills.rs`: `save_skill`,
  `list_skills`, `read_skill`, `delete_skill` — `allowed_tools`-gated like
  every builtin; registered in the main.rs registration chain.
- Injection: `build_system_prompt` (agents/agent) appends a `Skills:` block
  (name + when_to_use, capped at 20, cheap) to the memory-context area —
  the agent `read_skill`s when relevant.
- Tests: frontmatter round-trip, CRUD, invalid name, overwrite, empty store;
  tool round-trip (save → list → read → delete) + missing-param errors.

## Phase D — M1: per-agent metrics (~1 day) — ✅ DONE 2026-09-25
Implemented as planned (modular: one small module + hooks, no god class):
- `agents/metrics.rs`: `MetricsLog` rooted at `<wuffagent_home>/metrics/`
  (one JSONL file per agent, file name = sanitized agent name), line kinds
  `run {ts, tool_calls, tool_errors, verification_attempts, duration_ms,
  outcome}` + `feedback {ts, feedback: up|down}`; best-effort writes
  (warn-once, swallow), tolerant reads (corrupt lines skipped); test override
  `set_metrics_dir_for_testing`. Terminal outcome set: verified /
  verified_after_retry / gave_up / none (there is no terminal `needs_fix` —
  the nudge loop retries until pass or give-up; the verification-LLM-error
  default-pass records as `verified`). 7 unit tests.
- Writer: `run_llm_loop` end (loop.rs) — each handoff hop records its own
  line under its own agent; terminal outcome via new
  `VerificationState::final_outcome` (set in verify.rs: verified /
  verified_after_retry / gave_up / error-default-pass) because the attempt
  count alone is ambiguous (attempts==2 can be second-attempt pass OR
  give-up).
- Writer: `ui/chat_feedback.rs::remember_feedback` records up/down
  regardless of the memory store (metrics are always on).
- Reader: improver extraction prompt gets a "Recent metrics (…)" line
  (last 7 days, `MetricsSummary::format_line`) as deterministic evidence;
  agent editor UI (ui/agent_config.rs) shows all-time counts + last 5 lines
  for the selected existing agent.
- Tests: 7 metrics + 1 improver-prompt test (`MetricsDirGuard` — serialized
  on a process-wide lock, same pattern as the MCP config-path tests).
Gate: `cargo test --workspace` → core 611, egui 24, 1 doctest, EXIT=0.

## Priority & gates
A → B → C → D (A+B are small and independent; C before D so D's
summarizer can later include skill usage if ever wanted).
Each phase: `cargo test -p wuffagent-core` + `-p wuffagent-egui` +
workspace build; restart after core changes. Mark boxes in
`self-improvement-gaps.md` as phases land (and fix its stale T4 note).

## Housekeeping (any time)
- Update memory `fed257a1`: test_dangerous_command_blocked now passes
  (fixed in the shell test refactor).
- Update memory `dd09a41e`: pipeline-merge dead code is gone.
- Refresh memory `7ea4c897` status (T2/T3b/test_cmd/.prev done; this file
  is the remaining work list).
