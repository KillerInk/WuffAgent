# T3b: Runtime plugin (re)load — `reload_plugins` builtin

**Status:** planned
**Part of:** `plans/self-improvement-gaps.md` Phase 3, item T3 (the remaining half;
the `McpConfigChanged` event half landed as T3a, commit a0e9de8).

## Goal

Let the agent load native plugin tools at RUNTIME so it can extend itself with
new tools without a user-driven restart:

1. `reload_plugins` builtin — re-runs `ToolRegistry::discover_plugins()` over
   the registry's discovery paths and reports, per plugin file:
   - `loaded` (newly registered tool names),
   - `skipped` (already registered — tool name collision, which
     `registry.register` reports),
   - `failed` (load error text).
   The registry's `discover_plugins` currently returns only a count; the tool
   needs the detail, so the loader reports per-file results (additive change,
   count stays available for the startup call site).
2. `add_plugin_path` builtin — registers an EXTRA discovery path on the live
   registry (deduplicated), so a freshly built plugin can be loaded from a
   scratch dir without touching the default config-dir plugins path.
3. ABI docs in `AGENTS.md`: the two exported symbols
   (`wuff_tool_metadata` / `wuff_tool_create`), the tool trait contract
   (sync `execute`), and the full self-extend loop:
   write plugin crate → `cargo build --release` (shell) → copy the `.dll`
   into the plugins dir (or use `add_plugin_path`) → `reload_plugins`.

## Design decisions

1. **No registry API change for the tool itself**: `discover_plugins` already
   re-scans and re-`register`s; a name collision (plugin already loaded) is
   `Err(Validation)` and is REPORTED, not fatal — the tool aggregates
   per-file outcomes instead of aborting on the first error.
2. **Loader detail**: extend the plugin loader's report from `usize` to a
   per-file result list (path, status, message, tool names). `discover_plugins`
   keeps its `usize` return for the existing `main.rs` startup call (derived
   from the list) — or both call sites move to the detailed list; the detailed
   list is the single source of truth.
3. **`add_plugin_path` is additive + idempotent** (dedup against existing
   paths); it does NOT trigger a scan — the agent calls `reload_plugins` when
   it wants to load (one explicit step, predictable).
4. **Both tools are shared-registry tools** (like T1/T2): no per-execution
   state, registered in `main.rs` bootstrap via a new
   `builtin::register_plugin_tools(&registry)`, gated per profile by
   `allowed_tools` (add `reload_plugins` / `add_plugin_path` to the
   `wuffagent` profile's allowlist).
5. **Safety**: plugin load errors never crash the app (existing loader
   behavior: `libloading` errors are caught per file). A plugin that PANICS in
   `wuff_tool_create` is a process-level risk — accepted (documented in
   AGENTS.md); no sandboxing in this phase.

## Changes

| File | Change |
|---|---|
| `wuffagent-core/src/tools/dynamic/loader.rs` | per-file load report (path/status/message/tool names); `PluginLoadResult` list |
| `wuffagent-core/src/tools/registry.rs` | `discover_plugins` returns/aggregates the detailed list (count preserved for compat) |
| `wuffagent-core/src/tools/registry.rs` | `add_discovery_path(PathBuf) -> bool` (dedup, `true` if new) |
| `wuffagent-core/src/tools/builtin/plugins.rs` (new) | `ReloadPluginsTool` + `AddPluginPathTool` (+ `register_plugin_tools`) |
| `wuffagent-core/src/tools/builtin/mod.rs` | `pub mod plugins` + re-exports |
| `wuffagent-egui/src/main.rs` | `register_plugin_tools(&registry)` in bootstrap |
| `AGENTS.md` | plugin ABI section + self-extend loop |
| `agents/` (`wuffagent` profile in `~/.wuffagent/agents/`) | `allowed_tools += [reload_plugins, add_plugin_path]` |

## Verified (fill in when implemented)

- `cargo build` both targets clean; `cargo test -p wuffagent-core` green.
- Loader tests: a stub plugin (or the existing test fixture if any) loads,
  re-load reports `skipped`, a corrupt file reports `failed` without breaking
  the others.

## Out of scope

- Hot UNLOAD of plugin tools (registry has `unregister`; a `remove_plugin`
  tool is a natural follow-up but not needed for the self-extend loop).
- Plugin versioning / upgrade semantics.
- T4 (safer `restart`: `test_cmd` + `.prev` exe backup + resume-failure toast)
  — separate item.
