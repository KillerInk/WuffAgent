# Self-improvement loop: gap analysis + implementation plan

**Status:** SUPERSEDED (2026-09-26) — this was the first draft of the round-2
gap plan; phases 3–4 were re-planned as 3a–3c/4a–4c in
`self-improvement-gaps-2.md` (canonical status). Everything here is done except
the Phase 4b "effect check for non-latest markers" (LOW), which moved to
`self-improvement-gaps-2.md`'s explicit backlog.
**Companion to:** `plans/self-improvement-gaps.md` (2026-07, T-series) and
`autoplans/finish-self-improvement-loop.md` (2026-08-31 → 2026-09-26, A–D).
This file is the NEXT iteration: what is still missing from the loop that
WuffAgent improving itself.

## What the loop does today (verified in code, 2026-09-26)

1. **Evidence capture** — every agent turn records a `run` line into
   `MetricsLog` (`wuffagent-core/src/agents/metrics.rs`: tool_calls, tool_errors,
   verification_attempts, duration_ms, terminal outcome) and user feedback
   records a `feedback` line; lessons/outcomes are saved via `save_memory`
   with `agent:<name>` tags.
2. **Trigger** — `AgentEngine::execute_with_tools`
   (`wuffagent-core/src/agents/engine.rs`) calls `post_task_maintenance`
   (L268) after every task; the LLM improvement check runs when
   `auto_improve` (default ON, `MemoryConfig` L164-178) AND
   `completed % improvement_cooldown_tasks(5) == 0` AND
   `has_new_improvement_evidence()` (`memory/manager.rs` L172).
3. **Suggestion** — `suggest_improvements` (`memory/manager.rs` L82 →
   `build_improvement_prompt` `memory/improver.rs` L231) builds the prompt
   from: the agent's current system prompt, task + result, I1 RunStats
   trajectory, last 3 relevant lessons, rejected-suggestion history, and
   (M1) a `Recent metrics (…)` summary line (last 7 days,
   `MetricsSummary::format_line`, improver.rs:331). The LLM may propose
   prompt, `allowed_tools`, `reasoning_effort`, `shell_config`,
   `handoff_targets`, `task_timeout_ms`, or new agents
   (`types/policy.rs` ImprovementSuggestion).
4. **Review** — `AppEvent::ImprovementSuggested` → egui improvements panel
   (`ui/improvements/`): per-field approve toggles, editable prompt,
   evidence shown, revert-to-snapshot.
5. **Effect check (I5)** — `apply_improvement` records an
   `improvement-applied` marker (`applied_marker` memory); the next check
   includes `effect_check_section` (`memory/improver.rs` L109-144): the
   marker text + **the list of lesson entries since the marker timestamp**
   (`runs_since` at L127), and asks the LLM to state whether the change
   helped.

## Gaps (ranked by impact on self-improvement)

### A. The effect check ignores the strongest evidence (HIGH)
`effect_check_section` compares BEFORE/AFTER only via **new lessons** — the
weakest, least frequent signal. The metrics that already exist (outcome
distribution, tool-error rate, feedback) are computed for the improver's
7-day summary (M1) but NOT used for the before/after comparison of an
applied change. A prompt change that cut gave_ups in half is invisible to
the effect check if no new lesson was saved.
- `MetricsLog` has `summary_since(agent, since)` but NO `runs_since`/
  `summary_between` — there is no way to get the window BEFORE the marker.
- **Fix:** add `MetricsLog::runs_since(agent, since) -> Vec<RunRecord>`
  (and/or `summary_between(since, until)`); pass the marker timestamp into
  `build_improvement_prompt`; in `effect_check_section` render a before/after
  table (runs, gave_up share, tool-error rate, feedback up/down) and add a
  per-applied-change metrics line to `evidence` (improver.rs L335-364).

### B. The agent cannot inspect its own improvement state (HIGH)
The whole loop is passive: WuffAgent (the profile that edits WuffAgent)
gets no tool to (a) run a self-improvement check on demand, (b) see pending
suggestions, (c) see the last-check timestamp / why no check fired
(cooldown? no new evidence?). When it edits its own code (the main
self-improvement activity), it cannot feed the loop.
- **Fix:** new builtin tools in `tools/builtin/self_improve.rs`
  (one small file per tool, `Arc`-bound like the skills/memory tools):
  - `run_self_improvement(agent_name, focus) -> (status, suggestions)` —
    runs the same `suggest_improvements` path, bypasses cooldown (it is
    user/agent-initiated), and emits `AppEvent::ImprovementSuggested` so the
    panel still shows the result.
  - `list_improvement_status() -> { last_check, cooldown_tasks_left,
    pending_count, last_suggestion }` — reads the existing persisted state
    (`memory/manager.rs` improvement-state file, `ImprovementState` L524).

### C. No global (cross-agent) self-review (MEDIUM)
The check runs per the agent that just finished the task; cross-agent
problems (bad handoffs, duplicated skills, a profile whose metrics are
worse than a sibling's) never get a single view. `MetricsLog` is per-agent
filed precisely so this comparison is cheap.
- **Fix (cheap first step):** extend the per-task check's evidence with a
  one-line fleet summary (`MetricsSummary` across all agents, last 7 days)
  so the improver can see the agent in context; propose the full
  "fleet review" mode (LLM over the fleet table + all profiles, no single
  task context) as a follow-up `run_self_improvement(scope: "fleet")`.

### D. Skills (K1) have no maintenance/feedback path (MEDIUM)
`save_skill`/`list_skills`/`read_skill`/`delete_skill` exist, but nothing
tracks skill USE (a skill that is read but whose workflow keeps failing
should be revised), and the memory-maintenance pass does not touch skills.
- **Fix (phased):**
  1. Record skill reads into metrics (`kind: "skill_use", skill: name,
     agent: name`) — trivial append in `read_skill`, gives a usage count
     for free.
  2. Include per-agent `top skills by reads` + the agent's most recent
     lesson excerpts in the improver prompt's evidence.
  3. Let the improvement suggestion type grow an optional
     `skill_updates: Vec<(name, new_body_or_delete)>` and the panel apply
     them (small, mirrors the field-toggle pattern).
  (Full LLM skill-maintenance pass = stretch goal, after 1–3.)

### E. Effect check only covers the LATEST applied marker (LOW)
`latest_applied_marker` (improver.rs:327) — if two changes to one agent
were applied, only the newest gets an effect check; the older one is never
assessed. Acceptable for now; revisit if it bites.

### F. No regression/eval harness for prompts (LOW, big)
A prompt change is "good" only in the LLM's opinion. A proper loop would
replay a small set of golden tasks (or at least the task that triggered the
change) before/after. That is a project, not a phase — record here so it is
not lost.

## Phases

### Phase 1 — evidence & visibility (~1 day)
1a. `MetricsLog::summary_since` + `runs_since(agent, since) -> Vec<RunRecord>`
    — ✅ DONE 2026-09-26
    Implemented as `MetricsLog::summary_between(agent, start: Option<DateTime>,
    end: Option<DateTime>) -> MetricsSummary` (one bounded window; the effect
    check calls it twice — before and after the marker; `summary_since` is
    now a thin wrapper over it). Added the metrics comparison to `effect_check_section` (before
    window = [2w, 1w] before the marker, after = the last `window_days`) +
    run-count evidence line in `collect_evidence`. 3 new tests
    (`test_summary_between_windows` in metrics/tests.rs;
    `test_effect_check_before_after_metrics` +
    `test_effect_check_no_metrics_shows_no_data` in
    improvement/tests/effect.rs — the latter serializes on the shared
    `METRICS_DIR_LOCK` via `MetricsDirGuard` because `MetricsLog::default()`
    is process-global). Core 614→617, egui 24, build clean.
1b. ✅ DONE (folded into 1a): the marker timestamp was already threaded through
    to `effect_check_section` in the 1a commit; the before/after metrics lines
    render in the prompt section and the run counts in the evidence line.
1c. `list_improvement_status` tool (Gap B half; small, no LLM) — ✅ DONE
    2026-09-26
    Implemented modularly: `memory::ImprovementStatus` (types.rs) +
    `MemoryManager::improvement_status()` (read-only accessor over the
    persisted last_check, the live evidence gate, and the config) +
    `tools/builtin/improvement.rs` (`ListImprovementStatusTool`, no params,
    one small Tool struct) registered in `register_memory_tools`. Output:
    auto_improve on/off, cooldown setting, last-check timestamp (+~ago),
    new-evidence yes/no, lesson count. 3 tool tests (fresh-state, after
    check + new lesson, schema). `wuffagent` profile allowed_tools gained the
    tool (dogfood). Gate: core 617→620, egui 24, build clean (only the
    running-exe relink blocked in place, as usual).

Gate: `cargo test -p wuffagent-core` + `-p wuffagent-egui` + workspace
build; restart (core changed).

### Phase 2 — on-demand self-check + fleet context (~1 day) — ✅ DONE 2026-09-26
2a. `run_self_improvement` tool (Gap B) — ✅ DONE.
    - `tools/builtin/improvement.rs` → module `improvement/` (`mod.rs` +
      `status.rs` [1c tool moved] + `run.rs`), one file per tool.
    - `RunSelfImprovementTool { memory, agents, events }`: params `agent`
      (required) + `focus` (optional). Reuses
      `MemoryManager::suggest_improvements` (the manager already carries the
      memory-dedicated LLM client), so the on-demand check sees the SAME
      evidence as the per-task path (lessons, effect check, 7-day + fleet
      metrics). 180s backstop timeout via the web_search-style `block_on!`
      macro + cached `BLOCKING_RUNTIME`.
    - Emits `AppEvent::ImprovementSuggested` (session_id empty = app-level,
      routed by the UI to the active session — mcp-tools convention) so the
      suggestions land in the review panel; output lists the rationales.
    - Calls `record_improvement_check()` after the attempt (engine.rs
      semantics) so the evidence gate re-arms and list_improvement_status
      reflects the on-demand check. Explicit messages for auto_improve=off
      and unknown/missing agent (lists available profiles).
    - Registration: new `register_improvement_tools(registry, memory,
      agents, events)` in `improvement/mod.rs` (via the private
      `super::register_tool`); bootstrap hoists the AgentManager Arc and
      registers after the memory manager exists.
    - 5 tool tests (missing agent, unknown agent, no-LLM no-suggestions,
      auto_improve off, schema). No fake-LLM round trip: the LLM path is the
      one the per-task check already exercises in production; the tool adds
      no LLM-specific logic.
2b. Fleet summary line (Gap C step 1) — ✅ DONE.
    `MetricsLog::agent_names()` (dir scan, *.jsonl stems) +
    `fleet_summary_line()` in improvement.rs: one short line per agent with
    runs in the last 7 days, empty when nobody ran. In the prompt (slot
    after the per-agent metrics line) AND in the deterministic evidence.
2c. `description` field — ✅ DONE.
    `ImprovementSuggestion.description: Option<String>` (serde-defaulted;
    regression test: old JSON without the key still parses), in the improver
    field list + JSON template, in `PendingImprovement` (+
    `apply_description` toggle), editable single-line text in the panel
    (user edit wins, same pattern as the prompt buffer), applied in
    `apply_improvement_detailed` (reported as "description").

Gate: `cargo test --workspace` → core 626, egui 24, 1 doctest; clean check.
`wuffagent` profile allowed_tools gained `run_self_improvement` (dogfood).
Restart loads the new core (tools registered at bootstrap).

### Phase 3 — skills in the loop (1–2 days)
3a. `skill_use` metric line in `read_skill` + counts in `MetricsSummary`.
3b. Skill usage + lesson excerpts in improver evidence.
3c. `skill_updates` suggestion field + panel apply + memory bookkeeping
    (reuse the `applied_marker` pattern so skill changes ALSO get an
    effect check).
Gate: full workspace test; restart; dogfood by revising an existing skill
from inside a self-improvement run.

### Phase 4 — stretch (someday)
4a. `run_self_improvement(scope: "fleet")` full cross-agent review.
4b. Effect check for non-latest markers (Gap E).
4c. Golden-task replay harness (Gap F).

## Notes / constraints
- Keep it modular: new tools = one file per tool under `tools/builtin/`;
  no god classes; follow the `mcp/` split precedent.
- `MetricsLog::default()` is process-global — tests need
  `set_metrics_dir_for_testing` + a unique temp dir (see the metrics tests).
- Every phase ends with: gate tests, `git commit`, `restart` (core changed).
- `MemoryConfig.auto_improve` defaults ON; cooldown = 5 tasks; evidence
  gate = new lesson/outcome/feedback since last check (`memory/manager.rs`
  L172-207).
