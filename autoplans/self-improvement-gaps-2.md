# WuffAgent Self-Improvement — Gap Analysis Round 2 (plan, 2026-09-26)

**Status:** MOSTLY DONE (wuffagent; status log below is current, last updated in 123c02e).
Remaining: 2e min-interval knob + stretch F/I. Companion to `plans/self-improvement-gaps.md`
(round 1, 2026-07 — now fully implemented) and
`autoplans/finish-self-improvement-loop.md` (round 1's execution log, done
2026-09-25).

Round 1 built the *plumbing*: memory (fact/lesson/decision/context/goal +
tags), LLM memory maintenance, the skills store, per-agent metrics (JSONL),
a verification judge with nudge-retry, an auto-improvement check with
review panel, agent self-tools (edit_agent_profile, list_agents, restart,
handoff/hand_back), MCP management tools, runtime plugin reload, and the
I1–I5 improver upgrades (trajectory, profile fields, per-field approval +
evidence, cooldown, effect check).

This round audits what is **missing** in the self-improvement loop itself.

## Status log (wuffagent)

- **1a–1c, 2a–2c, 3a, 3b, 3c, 4a–4c: done** (git log `4312d98`…`46956c0` and earlier;
  see each commit). 3c note: history lives in `<skills_dir>/history/` (ts+seq names,
  keep-20, mirrors the agent pattern) with `revert_skill`; the panel got a per-skill
  Revert button (2-click armed).
- **2d: done 2026-07-11** (commit `46956c0`). Implemented as `scope: "fleet"` on the
  `run_self_improvement` tool (roster = name+description of every known profile,
  built by the caller) instead of a separate `agent: "all"` loop: ONE combined LLM
  call with a cross-agent prompt (shared failure patterns → skills / new shared
  agents, skill maintenance), evidence = `fleet_evidence_json` (per-agent metrics in
  the configured window + each agent's newest tagged lessons + fleet skill usage
  read/never-read, single JSON block), cost-gated (no signal → no call), state
  recorded under the pseudo-agent `"fleet"` (visible in `list_improvement_status` +
  the panel header). The panel's run-check selector gained a `fleet` entry.
- **2e: half done.** `improvement_metrics_window_days` (default 7) exists and is used
  by improver + `read_metrics`. `improvement_min_interval_hours` (wall-clock lower
  bound on top of the task cooldown) is NOT implemented.
- **Remaining:** 2e remainder (min-interval knob), stretch F (eval harness), stretch I
  (richer feedback). 3b's "trash dir" was superseded by 3c's history snapshots.

## What exists (verified in code, 2026-09-26)

- `agents/engine.rs` (336): single merged pipeline
  (`execute_with_tools` → `post_task_maintenance`, L268–332).
  `improvement_due` (free fn, L54–56) uses ONE shared `tasks_completed`
  counter (L74) — the cooldown is **global across agents**, and the
  improver analyzes **only the agent that just finished**.
  `has_new_improvement_evidence()` (memory/manager.rs:172) +
  `record_improvement_check()` (:191) + `ImprovementState` (:524,
  `last_check: Option<i64>`) gate the LLM call.
- `agents/improvement.rs` (414): the improver.
  `suggest_improvements` (L184): lessons via `collect_lessons` (gated on
  `improvement_trigger_lessons`, default 1), `cap_lessons` budget,
  `rejected_history` (F5), `trajectory_line` (I1), 7-day metrics line
  (L223–227, `MetricsLog::summary_since(...).format_line()`),
  `effect_check_section` (L109–144, I5), prompt template with the
  changeable-field list (L265–272) + JSON template (L274–291),
  `TOTAL_PROMPT_CHAR_BUDGET` tail truncation, evidence attached
  deterministically (L362–384).
- `agents/metrics.rs` (455): `MetricsLog` — one JSONL per agent
  (`<wuffagent_home>/metrics/<name>.jsonl`), lines `run {ts, tool_calls,
  tool_errors, verification_attempts, duration_ms, outcome}` +
  `feedback {ts, up|down}`. Readers: `read_all`/`recent`/`summary_since`
  (L381, → `MetricsSummary {runs, tool_calls, tool_errors, verified,
  verified_after_retry, gave_up, not_verified, feedback_up,
  feedback_down}` + `format_line()`). Writers: `record_run` (from
  `run_llm_loop`) + `record_feedback` (chat thumbs). No token/cost line.
- `memory/manager.rs` (544): store CRUD, `suggest_improvements` wrapper
  (L82), evidence gate, maintenance. `MemoryEntry` carries
  `timestamp: Option<DateTime<Utc>>` — the I5 applied-marker is already
  machine-readable (improvement.rs:111).
- `memory/skills.rs` (11 KB) + `tools/builtin/skills.rs`: SkillStore —
  CRUD + `prompt_block` injection (name/description/when_to_use, capped
  at 20) into every system prompt. **No usage tracking, no maintenance.**
- `types/policy.rs`: `ImprovementSuggestion` — agent_name, prompt_change,
  rationale, new_agents, allowed_tools, reasoning_effort, shell_config,
  handoff_targets, task_timeout_ms, evidence. No `description` field.
- `ui/improvements/` (egui): review panel — per-field toggles, editable
  prompts, Revert (F4), evidence display; approve → profile write in the
  profile's actual dir + I5 marker; dismiss → F5 rejection lesson.
  `ImprovementsPanel.pending` is **in-memory only** (mod.rs:113–117) —
  suggestions vanish on app exit if unreviewed.
- Agent self-tools: list_agents, edit_agent_profile (history snapshots),
  restart (build-gated, dual-target), handoff/hand_back (in-turn +
  sub-session), memory/skill tools, MCP management, plugin reload,
  web_search/fetch_url. No tool for **reading metrics** or **triggering
  the improver on demand**.

## Gaps (ordered by impact)

### A. The effect check ignores the metrics it already collects (HIGH)
`effect_check_section` (agents/improvement.rs:109–144) lists only **lesson
memories** newer than the applied marker. The strongest outcome signals —
verification outcomes, tool error rate, user thumbs — are in `metrics/<a>
.jsonl` but never compared before/after. The "did this prompt change
help?" question is answered from text, not data.

### B. No agent-initiated self-review (HIGH)
Agents can edit their own profile, restart, save memories/skills — but
cannot inspect their own metrics, and the improver runs **only** on the
global cooldown after a task of the *current* agent. A "coder keeps
handing back to wuffagent" pattern is only analyzed when coder finishes a
task AND the global counter lands on a boundary AND new lesson evidence
arrived.

### C. No cross-agent / global review (HIGH)
Round 1's round-2 roadmap said "per-agent + global self-review"; global
never landed. Cross-agent problems (bad handoff targets, overlapping
agents, a profile that should exist) can only be proposed as `new_agents`
side-proposals while analyzing one agent.

### D. Agents can't see their own metrics (MEDIUM)
No `read_metrics` tool. The agent editor UI shows them to humans; the
agent itself is blind to its own run history (the improver gets it, but
that's a different LLM call with a different prompt).

### E. Skills are append-only (MEDIUM)
Skills are created but never: (1) tracked for usage (did read_skill lead
to a successful task?), (2) maintained (no stale-skill cleanup, no
consolidation — the LLM maintenance pass covers memories only), (3)
versioned (overwrite = silent replacement, no history, unlike
edit_agent_profile's prompt history).

### F. No eval / regression harness (MEDIUM, stretch)
Prompt changes are judged by vibes (lessons) + a weak effect check (A).
No saved test tasks per agent, no before/after scoring. A minimal version
needs almost no new machinery: a JSONL of saved tasks + run them through
the existing verification judge + record to metrics.

### G. Suggestion lifecycle leaks (MEDIUM)
1. Pending suggestions are not persisted (app exit loses them; the
   evidence gate may never re-arm them).
2. `ImprovementSuggestion` has no `description` field — the improver can't
   fix an agent's description (which is what agent *selection* reads).
3. No visibility into WHY the auto-check is idle (cooldown? no new
   evidence? auto_improve off?) — the panel only shows pending items.

### H. Cost control is coarse (LOW)
- The 7-day metrics window is hardcoded (improvement.rs:225) — no
  `improvement_metrics_window_days` in MemoryConfig.
- Cooldown is per *task completion*, not wall-clock (a day with 50 tasks
  triggers 10 checks; a quiet week triggers none even if evidence
  accumulated — mitigated by the evidence gate, but not eliminated).
- Metrics have no token/cost lines, so the cost of the improvement
  system itself (improver LLM call + verification calls) isn't visible in
  the data it produces.

### I. Feedback loop is binary (LOW, stretch)
Thumbs up/down per message only. No per-tool-error feedback, no
"this suggestion was right" signal beyond approve/dismiss.

## Plan

### Phase 1 — make the loop measure what it changes (1–2 days)
- **1a. Metrics-backed effect check (fixes A).**
  In `effect_check_section` (agents/improvement.rs): also compute
  `summary_since(marker.timestamp)` and the matching BEFORE window
  (window length = time since marker, capped at 30 days) via a new
  `MetricsLog::summary_between(agent, start, end)` (trivial over
  `read_all`; `summary_since` keeps working). Section text becomes:
  metrics before vs after (runs, error rate, verified/gave-up, thumbs)
  + the lesson list. No new MemoryEntry fields needed — the marker's
  `timestamp` already exists (I5/G8d resolved).
  Tests: synthetic JSONL before/after a marker → section contains both
  windows; missing metrics file → section still works (lessons only).
- **1b. Persist pending suggestions (fixes G.1).**
  New tiny module `memory/pending_improvements.rs` (or egui-side, but core
  is better: survives UI rework): `<wuffagent_home>/pending_improvements.
  json`, one file per project or global; `ImprovementsPanel` loads on
  startup, saves on any mutation (append/refresh/approve/dismiss).
  Serde-compatible with the existing `ImprovementSuggestion` (add
  `#[serde(default)]` where needed — already there).
  Tests: round-trip, corrupt file → empty, approve removes the entry.
- **1c. `description` as a changeable field (fixes G.2).**
  Add `description: Option<String>` to `ImprovementSuggestion` (serde
  default), the improver's field list + JSON template
  (improvement.rs:265–291), `PendingImprovement` (+toggle, default on),
  and the apply path (ui/improvements/memory.rs: `apply_improvement_
  detailed` → `config.description`).

### Phase 2 — agent-initiated + cross-agent review (2–3 days)
- **2a. Per-agent improvement state (foundation for B+C).**
  `ImprovementState` (memory/manager.rs:524): `last_check: Option<i64>`
  → `per_agent: BTreeMap<String, {last_check: i64, completed: usize}>`
  (+ keep `last_check` as the max for back-compat / evidence gate).
  `improvement_due` (engine.rs:54) becomes
  `improvement_due_for(agent, state, cooldown)`; the shared
  `tasks_completed` stays for the maintenance cooldown only.
- **2b. `check_improvements` tool (fixes B, part of C).**
  New builtin (`tools/builtin/improve.rs`, registered like the memory
  tools; allowlisted per agent — enable for wuffagent first):
  `check_improvements(agent: Option<String>, focus: Option<String>)` →
  runs the EXISTING `MemoryManager::suggest_improvements`
  (memory/manager.rs:82) for the named agent (default: the calling
  agent), bypassing the cooldown (agent-initiated = explicit) but
  respecting the evidence gate + `auto_improve`, and returns the
  suggestions as tool output (so the agent can summarize/act) AND emits
  `AppEvent::ImprovementSuggested` (so the panel gets them too).
  Reuses `collect_lessons`/metrics/effect-check unchanged — only the
  trigger changes. Guard: one check per agent per N minutes (in-memory,
  best-effort) so a chatty agent can't spam the LLM.
- **2c. `read_metrics` tool (fixes D).**
  New builtin (`tools/builtin/read_metrics.rs`): `read_metrics(agent:
  Option<String> (default caller), days: Option<u32> (default 7))` →
  `summary_since(...).format_line()` + the last N raw lines
  (`recent(agent, n)`). Read-only, no LLM. One small Tool struct, tests
  with `set_metrics_dir_for_testing` (existing pattern).
- **2d. Global review on demand (fixes C).**
  Extend `check_improvements` with `agent: "all"` (or a `scope:
  "global"` param): loop all enabled profiles, collect per-agent
  evidence (lessons + 7-day metrics + rejection history), ONE combined
  LLM call with a cross-agent prompt section ("handoff patterns,
  overlapping/missing roles") → suggestions for any agent incl.
  `new_agents`. Keep it ONE call (cost). Config:
  `improvement_global_enabled: bool` (default true, tool-gated anyway).
- **2e. MemoryConfig knobs (fixes H.1/H.2).**
  `improvement_metrics_window_days: u32` (default 7; used in
  improvement.rs:225 instead of hardcoded), optional
  `improvement_min_interval_hours: u64` (default 0 = off; wall-clock
  lower bound on top of the task cooldown — `ImprovementState` already
  stores timestamps).

### Phase 3 — skills become first-class improvement memory (1–2 days)
- **3a. Skill usage line in metrics (foundation for E).**
  `read_skill` tool records `skill {ts, name, used: bool}` in the
  existing per-agent JSONL (new `MetricsLine` variant; serde-tolerant so
  old lines still parse). `used: true` is recorded when the agent calls
  `save_skill`/`delete_skill` for the same name within the same session,
  OR more simply: count reads only (usage = "agent thought it relevant");
  start with reads-only, keep the `used` field for later.
- **3b. Skill maintenance (fixes E).**
  Extend the LLM maintenance pass (memory/maintenance.rs) with a
  skills sub-pass (same batching pattern): input = skill metas + their
  usage counts (3a) + ages; output actions: merge/retire stale skills,
  propose updated `when_to_use`. Retires go to a trash dir (like the
  memory maintenance's supersede handling) instead of delete.
- **3c. Skill history (fixes E.3, small).**
  On `save_skill` overwrite, copy the old file to
  `<skills_dir>/<name>.history/<utc-timestamp>.md` (capped at 5, like
  agent prompt history). No new UI needed; `read_skill` gains an
  `at: Option<String>` (timestamp) param to view history.

### Phase 4 — visibility (½–1 day, mostly egui)
- **4a. Improvement status in the panel (fixes G.3).**
  Header line under "Self-Improvement Suggestions": last check time,
  cooldown progress (`completed % N`), evidence-gate state,
  auto_improve flag — all from `MemoryManager` accessors (add
  `improvement_status() -> String` in core so egui stays dumb).
- **4b. "Run check now" button** → calls the same path as 2b for the
  selected agent (bypasses cooldown).
- **4c. Metrics tokens line (fixes H.3, small).**
  If the LLM client already reports token usage (usage/recorder exists):
  add `tokens_in/tokens_out` to the `run` metrics line (Option fields,
  serde-defaulted). Improver prompt can then quote cost.

### Stretch (not scheduled)
- **F. Eval harness:** `evals/<agent>/<task>.json` (task text + expected
  verification criteria); a `run_evals` tool/UI action executes them
  headlessly through the existing verification judge and writes an
  `eval` metrics line; effect check (1a) can then show eval deltas.
- **I. Richer feedback:** per-tool-error thumbs + "suggestion helped"
  follow-up question after an approval (feeds F5-style lessons).

## Explicit backlog (out of scope here)
- Effect check only covers the LATEST applied marker (LOW; dropped from the
  first draft `self-improvement-loop-gap-plan.md` Phase 4b — revisit if it bites).
- Typed `LlmError` (known since step 20; cross-cutting).
- `agents/manager.rs` (605) / `memory/maintenance.rs` (566) / egui
  `agent_config.rs` (673) splits — do when touched (see
  autoplans/code-design-improvements.md "Remaining backlog").
- Skill *injection* relevance ranking (currently all ≤20 skills are
  listed; fine at current counts).

## Order & gates
1 → 2 → 3 → 4 (Phase 1 is independent and small; Phase 2 is the core of
this round). Each phase: `cargo test -p wuffagent-core` + `-p
wuffagent-egui` + `cargo build --workspace` green, no warnings, one
commit each; restart WuffAgent after core changes (dual-target
self-restart).

## Notes / hazards
- The M: repo drive has dropped untracked files mid-session this round
  (`agents/wuffagent.json`, `autoplans/self-improvement-gaps.md` both
  vanished after being listed) — commit new files early.
- The machine clock reads 2026-09-26 while some file mtimes show
  2026-10-06 (future) — treat mtimes from this drive with suspicion;
  `git log` dates are the reference.
