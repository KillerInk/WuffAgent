# T1: Agent-profile tools (`list_agents` / `edit_agent_profile`)

**Status:** implemented
**Part of:** `plans/self-improvement-gaps.md` Phase 3 (first-class self-modification).
Phases 0–2 are done; T1 is the first Phase-3 item and unblocks the rest
(the agent can now manage profiles instead of raw `write_file` on `agents/*.json`).

## Goal

Validated, reversible, single-writer tools for agent profiles:

- `list_agents` — name, description, enabled, path, allowed_tools,
  handoff/restart flags, prompt size/preview for every known profile
  (incl. disabled ones — an editor must be able to re-enable).
- `edit_agent_profile` — change any profile (including the caller's own):
  `description`, `system_prompt` (full replace), `allowed_tools` (full
  replace), `enabled`, `task_timeout_ms`, `reasoning_effort`,
  `handoff_enabled`, `handoff_targets`, `restart_enabled`, `shell` (object),
  `new_name` (rename). Omit a field = unchanged.

## Design decisions

1. **Shared registry tools**, not per-execution (unlike `handoff`/`restart`):
   they only need the `AgentManager` discovery set, which is static for the
   process lifetime. Registered once in `wuffagent-egui/src/main.rs` via a new
   `builtin::register_agent_tools(registry, Arc<AgentManager>)`, using the SAME
   dir set the UI selector uses (`~/.wuffagent/agents` + `./agents` + `<exe>/agents`).
2. **Gated by `allowed_tools`** like every other tool (empty list = all tools).
   No new `*_enabled` flag.
3. **In-place writes (F3 pattern)**: the profile's ACTUAL file may live in a
   search dir, and may even be named differently from its `name` field
   (real example: `agents/general.json` holds `name: generalist`). The tool
   resolves `(dir, file, config)` with a new core helper and writes through an
   `AgentManager` bound to that dir, so the F4 history snapshot lands next to
   the profile — exactly like the UI approve path. `AgentManager` stays the
   only writer.
4. **Canonicalize oddly-named files on edit**: if the file is not at the
   conventional `<dir>/<name>.json` path, snapshot it and rename it to the
   conventional path before `edit_agent` (otherwise the edit would create a
   second file with no snapshot and leave the original stale).
5. **No self-elimination**: removing `list_agents` or `edit_agent_profile`
   from the CURRENT `allowed_tools` requires an explicit `allow_self_removal:
   true` (default false). A shared tool instance can't know the calling
   agent's name (the `Tool` trait has no caller context), so the guard is
   "actual removal needs acknowledgment" instead of "own name only" — that
   also protects other agents' self-mod capability.
6. **UI refresh needs no event**: the chat agent selector (`get_agent_names`)
   re-scans the agents dirs every frame, so an edit shows up immediately.
7. **Timing contract** (in tool description + result): edits apply from the
   NEXT message — the running agent's prompt/tool list were fixed when its
   turn started (policy resolved at send time).

## Changes

| File | Change |
|---|---|
| `wuffagent-core/src/agents/config.rs` | `parse_agent_file` (private; new+legacy parse, no enabled filter), `list_agent_files(dir) -> Vec<(PathBuf, AgentConfig)>` (incl. disabled), `find_agent_file(dirs, name) -> Option<(dir, file, cfg)>` (first dir wins); `load_agent_from_dir` refactored onto `parse_agent_file` |
| `wuffagent-core/src/agents/manager.rs` | `snapshot_file` (private split of `snapshot_agent`); `canonicalize_profile_file(name, actual) -> Result<PathBuf>` (snapshot + rename to conventional path; no-op when already conventional/missing) |
| `wuffagent-core/src/tools/builtin/agent_profile.rs` (new) | `ListAgentsTool`, `EditAgentProfileTool` (both `Arc<AgentManager>`), name validation (no path separators — names become filenames), rename collision check, self-removal guard, result JSON with applied-fields list + timing note |
| `wuffagent-core/src/tools/builtin/agent_profile/tests.rs` (new) | list shape/dedup/disabled/legacy; edit in primary + in search dir; rename; collision; guard on/off; validation errors; canonicalize of oddly-named file; `find_agent_file` semantics |
| `wuffagent-core/src/tools/builtin/mod.rs` | `pub mod agent_profile` + `register_agent_tools` |
| `wuffagent-egui/src/main.rs` | build `AgentManager` (primary + search dirs already computed) and call `register_agent_tools` before the engine is built |
| `AGENTS.md` | new bullet: T1 tools, registration, gating, in-place/canonicalize, guard, next-message timing |
| `~/.wuffagent/agents/wuffagent.json` | add both tools to own `allowed_tools` (dogfood) |

## Out of scope (later autoplans)

- T2 MCP management tools, T3 runtime plugin reload, T4 safer restart —
  separate items in the gap plan.
- Emitting an `AppEvent::AgentsChanged` — unnecessary while the selector
  re-scans every frame; revisit if a cache is ever introduced.

## Verification

- `cargo test -p wuffagent-core` (new tool tests + existing manager/config tests),
  `cargo test -p wuffagent-egui`, `cargo build` clean.
- Dogfood: after restart the running `wuffagent` profile lists/edits profiles.
