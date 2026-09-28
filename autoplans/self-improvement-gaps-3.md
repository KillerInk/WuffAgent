# WuffAgent Self-Improvement — Round 3 Gap Plan (2026-09-28)

**Status:** OPEN (wuffagent). Companion to the round-2 canonical status in
`self-improvement-gaps-2.md` (DONE except stretch F + I — both promoted into
this plan) and `loop-gap-plan.md` (superseded, all items done).

## What the loop does today (verified in code + live state, 2026-09-28)

- Per-task trigger: `MemoryManager::agent_improvement_due` (per-agent task
  cooldown × no-op backoff × wall-clock min interval × evidence gate), then
  `suggest_improvements` → review panel (persisted via `pending_store.rs`) →
  apply with I5 marker + deterministic `effect_verdict`.
- Evidence in the improver prompt: tagged lessons, 7-day + fleet metrics
  lines, skill usage/retire lines, before/after metrics windows around the
  latest applied marker, rejection history, low-sample caveat
  (`improvement_min_samples`).
- Live state (`list_improvement_status`, 2026-09-28): coder + researcher
  verdicts stuck at **"inconclusive (no runs after the change)"** — the
  effect check re-runs on every check, re-records "inconclusive", and feeds
  the LLM a preliminary-sample section while 0 runs exist; generalist has
  **never** been checked (1 task since; 5-task cooldown makes rare agents
  starve); "new evidence since last check: yes" — the evidence gate is
  purely **lesson/feedback-driven**, so metric-only degradation (error rate
  doubling with no saved lesson) never re-arms the check.

## Gaps (ordered by impact)

### A. The loop only wakes up for *textual* evidence (HIGH)
`has_new_improvement_evidence()` fires on new lesson/feedback entries since
the last check. An agent whose tool-error rate doubles but who keeps
"working fine" and saves no lesson is never re-examined — the metrics that
would reveal it are collected, read by the improver, but never by the
*trigger*. The loop improves on complaints, not on data.

### B. Effect checks burn budget and noise on un-judgeable changes (HIGH, cheap)
`effect_check_section` (agents/improvement.rs:177) runs whenever a marker
exists, even with `after.runs == 0`: it renders the full before/after
section + "(none yet)" outcomes + a low-sample note into the LLM prompt and
re-records `last_effect_verdict = "inconclusive (no runs after the change)"`
on every check. Live symptom today for coder + researcher. The verdict
field therefore oscillates/never settles, and `list_improvement_status`
reports "inconclusive" as if it were a result.

### C. No eval / regression harness (MEDIUM→HIGH, stretch F promoted)
Prompt/profile changes are still judged by LLM vibes + before/after run
metrics (small samples, confounded by task mix). No saved golden tasks, no
`evals/` dir exists in the repo or home. The round-2 note: "a minimal
version needs almost no new machinery: a JSONL of saved tasks + run them
through the existing verification judge + record to metrics."

### D. The loop's own cost is invisible (MEDIUM, cheap)
Improver LLM calls (per-task + on-demand + fleet) consume tokens that never
appear in the metrics they produce — the loop can't answer "what does
improving itself cost per week?" `MetricsLine::Run` has tokens (4c done),
but there is no `check` line.

### E. Feedback is binary and one-shot (MEDIUM, stretch I promoted)
Per-message thumbs only. No signal that an *applied improvement* was right
(the effect check asks an LLM; the user never confirms), no
"helpful/unhelpful" on the applied record feeding F5-style rejection
lessons. And the 2e note: "auto-apply low-risk suggestions" was deliberately
not added — belongs in a plan, now this one.

### F. Cross-agent intelligence is thin (LOW–MEDIUM)
- Fleet review (2d) exists but its evidence does **not** include
  `last_effect_verdict` per agent — "change X improved agent A, agents
  B/C share the same profile shape → propose the same" (cross-session
  lesson propagation, round-2 out-of-scope item) is not expressible.
- Effect check still covers only the **latest** applied marker per agent
  (round-1 gap E / round-2 explicit backlog): the second-newest change is
  never assessed.
- Rare agents starve the per-task cooldown (generalist: 1 task, never
  checked); fleet checks are the only safety net and are manual/on-demand.

## Plan

### Phase 1 — make the loop data-driven and quiet where un-judgeable (1 day)
- **1a. Metrics-driven evidence (fixes A).**
  In `MemoryManager::has_new_improvement_evidence` (or a new
  `agent_evidence` alongside it): also compute
  `MetricsLog::summary_since(agent, last_check)` vs the matching preceding
  window (reuse `summary_between`, the same before-window rule as
  `effect_check_section`). New evidence when ANY of:
  - runs in window ≥ `improvement_metric_evidence_runs` (new MemoryConfig,
    default 3 — so 1–2 runs don't re-arm the LLM), AND
  - tool-error-rate delta ≥ 5pp, OR gave_up share delta ≥ 20pp, OR
    avg-duration delta ≥ 50% (constants for v1; knobs only if tuning asks).
  Keep the lesson/feedback gate (OR-combined) — lessons stay the fast path.
  Surface the trigger reason in `list_improvement_status` ("new evidence:
  yes (metric delta: err 12%→34%)") so the loop is explainable.
  Tests: synthetic JSONL with flat metrics → no re-arm; error-rate jump →
  re-arm; below run floor → no re-arm; lessons-only still re-arms.
- **1b. Defer + settle effect checks (fixes B).**
  In `effect_check_section`: when `after.runs < min_samples.max(1)`,
  (a) emit a ONE-LINE section instead of the full before/after block
  ("change applied N days ago, only M run(s) so far — not yet judgeable;
  do not propose a revert on this"), (b) do NOT record a new
  `last_effect_verdict` (keep the previous value or `None` → status shows
  "awaiting samples (M/min)"), and (c) add `awaiting_samples:
  (have, need)` to the per-agent status view. When the floor is crossed,
  the first full check becomes THE judgment; optionally record
  `verdict_judged_at` so a re-judgment with more data is labeled
  "re-check" in status (nice-to-have, keep simple).
  Tests: 0 runs → short section, verdict unchanged; ≥ min_samples → full
  section + verdict recorded (extend the existing low-sample test).
- **1c. Loop cost line (fixes D).**
  New `MetricsLine` variant `Check { ts, agent, scope: "agent"|"fleet",
  tokens_in, tokens_out, suggestions: usize, duration_ms }` (serde-tolerant
  like 4c) written by `MemoryManager::run_improvement_check` (the single
  choke point used by the per-task path, the `run_self_improvement` tool,
  and the panel button). `MetricsSummary` counts checks + their tokens;
  `list_improvement_status` appends "loop cost (7d): N checks, X tok in /
  Y tok out, S suggestions". Tests: line round-trip, summary aggregation,
  status rendering.
  Gate after phase: `cargo test --workspace` green, no warnings, one commit
  per item, restart (core changed).

### Phase 2 — eval harness (stretch F; 2–3 days)
- **2a. Eval definitions (core-only).**
  `<wuffagent_home>/evals/<agent>.jsonl` — one JSON object per line:
  `{id, task, expect (verification criteria text), max_tool_calls?: usize}`.
  `memory/evals.rs` (mirror the `skills.rs` module shape): `load`, `save`
  (upsert by id), `list(agent)`, `delete(agent, id)` + test override
  `set_evals_dir_for_testing`. Tolerant parsing (skip corrupt lines).
  Tools: `save_eval` / `list_evals` / `delete_eval` in
  `tools/builtin/improvement/evals.rs` (allowlisted per agent; wuffagent
  dogfoods first). Tests: round-trip, upsert, corrupt line, empty dir.
- **2b. `run_eval` (the engine part).**
  New builtin `run_eval(agent, eval_id?, all?: bool)`: loads the eval(s),
  for each: builds the agent's config, calls the existing engine path
  headlessly (`execute_with_tools` with the eval task), runs the existing
  verification judge against `expect`, records a `MetricsLine::Eval { ts,
  agent, id, passed, score?, duration_ms, tokens_in/out }` line.
  Serial execution, per-eval timeout (reuse the 180s `block_on!` pattern).
  Output: pass/fail table + one-line verdict per eval.
  NOTE: this is the one genuinely new code path (engine already supports
  headless runs — verify the exact entry point during implementation; if
  `execute_with_tools` needs a live session id, add a minimal
  `run_headless(config, task)` wrapper in `agents/engine.rs` rather than
  faking a session).
- **2c. Evals in the effect check (the payoff).**
  `effect_check_section` gains an evals window: pass/fail + scores before
  vs after the marker (same `summary_between` treatment over `Eval`
  lines). An eval regression is now *data* the improver can act on —
  this is what upgrades "prompt vibes" to "before/after scoring".
- **2d. Panel + dogfood.**
  Agent editor shows per-agent eval results (last run, pass rate);
  "Run evals" button reusing the `ImprovementCheckFinished`-style status
  line pattern. Seed 2–3 golden tasks for `wuffagent` itself (a
  self-restart flow, a metrics read, a memory save/search) and 1–2 for
  `coder` — the fleet's first regression baseline.
  Gate: workspace green; restart; run the seeded evals once live and
  commit the resulting pass/fail as the baseline.

### Phase 3 — richer feedback + auto-apply (stretch I; 1–2 days)
- **3a. "Did this help?" on applied improvements.**
  Applied improvements (panel history / the I5 marker record) gain a
  two-state "helpful / not helpful" action (egui, persisted in
  `improvement_state.json` per-agent `last_helpfulness`). "not helpful"
  writes an F5-style rejection lesson (tag `agent:<name>`, so the evidence
  gate re-arms and the next check knows) and feeds the fleet review.
  "helpful" marks the verdict as user-confirmed (shown in status).
- **3b. Auto-apply low-risk suggestions (the 2e deferred item).**
  `MemoryConfig.improvement_auto_apply: "off" | "low_risk"` (default off,
  serde-defaulted). Low-risk = `description`-only suggestions (no prompt,
  no tools, no shell) — apply immediately via the existing
  `apply_improvement_detailed` path + I5 marker, emit a dismissible toast
  "auto-applied description change — revert available", still show in the
  panel (marked auto-applied, no per-field toggles). Prompt/tools/shell
  changes NEVER auto-apply. Tests: off = no auto-apply; low_risk applies
  description-only and not prompt-only; marker recorded.
- **3c. Per-run feedback (cheap add, optional).**
  The agent editor's last-5-lines view: a 👍/👎 per recent `run` line →
  `feedback {ts, run_ts, up|down}` (serde-tolerant; `MetricsSummary`
  counts run-level feedback separately). Gives the loop task-level
  sentiment instead of message-level only.

### Phase 4 — fleet intelligence (1 day)
- **4a. Verdict-aware fleet evidence.**
  `fleet_evidence_json` gains per-agent `last_effect_verdict` +
  auto-apply/auto-applied flags; the fleet prompt explicitly asks: "if a
  change improved one agent, name sibling agents with similar roles and
  propose the same change (cross-session lesson propagation)".
- **4b. Non-latest marker effect checks (round-1 gap E, promoted).**
  Persist a small `judged_markers: [(marker_ts, verdict)]` list (cap 3)
  in the per-agent improvement state; the section lists prior verdicts in
  one line each so a re-applied-then-regressed sequence is visible, and
  the latest-marker rule stays (no prompt bloat).
- **4c. Rare-agent safety net (fixes F-starvation, cheap).**
  In the fleet check, flag agents whose `runs_since_check` > 0 but whose
  last check is older than `improvement_stale_days` (new config, default
  7) or never ran — one prompt line: "these agents have unreviewed
  activity: …". No new trigger path; the existing manual/fleet check
  becomes the catch. (Alternative — a wall-clock per-task fallback — is
  deliberately deferred: more moving parts for a rarer problem.)
- **4d. Skill injection relevance ranking (round-2 backlog, only if 20+
  skills bite): skip for now; noted to stay tracked here.**

## Explicit out of scope (tracked elsewhere)
- Typed `LlmError` (known since step 20; cross-cutting, own plan).
- God-file splits (`agents/manager.rs`, `memory/maintenance.rs`, egui
  `agent_config.rs`) — split when touched, per
  `autoplans/code-design-improvements.md`.
- Per-tool-error thumbs (3c covers the task-level slice; per-bubble UX
  stays stretch).

## Order & gates
Phase 1 → 2 → 3 → 4. Phase 1 is independent and small (do it first — it
directly fixes the live "inconclusive" + never-wakes symptoms). Each
phase: `cargo test -p wuffagent-core` + `-p wuffagent-egui` + workspace
build green, no warnings, one commit per item, restart WuffAgent after
core changes (dual-target self-restart; verify which build is live
afterwards). Commit this plan immediately after writing (M: drive
drops untracked files).

## Verification targets (definition of done for the round)
- `list_improvement_status` shows a settled verdict (or "awaiting samples
  (M/min)") for every agent with a marker — no more perpetual
  "inconclusive (no runs after the change)".
- A synthetic error-rate jump re-arms the evidence gate with no new lesson.
- `list_improvement_status` reports 7-day loop cost (checks + tokens).
- `wuffagent` has ≥ 3 seeded golden evals; `run_eval` produces a pass/fail
  table and an `Eval` metrics line; the effect check quotes eval deltas.
- A `description`-only suggestion auto-applies under `low_risk` and is
  reversible from history.

## Notes / hazards
- `MetricsLog::default()` is process-global — new line-kind tests need
  `set_metrics_dir_for_testing` + a unique temp dir (existing pattern).
- 2b is the only phase that may need an engine seam (`run_headless`);
  confirm against `agents/engine.rs` before coding — prefer the minimal
  wrapper over plumbing a fake session through `execute_with_tools`.
- Keep new config serde-defaulted (no migration), following
  `improvement_min_samples` / `improvement_min_interval_hours`.
- Machine clock vs mtime drift on the M: drive is real (round-2 note);
  `git log` dates are the reference.
