# Self-improvement gaps — round 2 (2026-10-06)

**Status:** planned (wuffagent)
**Supersedes:** `self-improvement-gaps.md` (round 1, implemented 2026-08-31 → 09-25) and the
self-extend additions T1–T4 (`t1-…md`, `t2-…md`, `t3-…md`, `t3b-…md` — all done; T3b marked
DONE in this round's housekeeping).

## Inventory — what the loop already has (verified in code, 2026-10-06)

1. **Episodic memory**: entries + typed lessons, token-dedup, fuzzy injection (top 5 / 1000
   chars), maintenance LLM pass (dedup/retag/supersede/revive, opt-in, threshold 40),
   per-agent lesson tagging convention (`agent:<name>`).
2. **Procedural memory (skills)**: `SkillStore` (`memory/skills.rs`), 4 tools, `═══ SKILLS ═══`
   prompt block for every agent (capped 20), tool guidance gated on `save_skill` in
   `allowed_tools`.
3. **Per-agent metrics (M1)**: `agents/metrics/` — JSONL per agent (`run` lines: tool
   calls/errors, verification attempts, duration, terminal outcome; `feedback` up/down lines),
   undercount bugfix landed (D-bis), `MetricsLog::recent/summary_since/last_7_days_summary`,
   `describe()`, `MetricsSummary::format_line`.
4. **Post-task improvement check (I1–I5)**: `agents/improvement.rs` — LLM call with
   trajectory (RunStats), lessons, recent metrics, rejection lessons, effect-check section for
   the last applied prompt; I2 profile-field proposals (allowed_tools, reasoning_effort,
   shell_config, handoff_targets, task_timeout_ms), I3 evidence attached to each suggestion,
   I4 throttle (cooldown 5 tasks + new-evidence gate, `ImprovementState` persisted in the
   memory dir).
5. **Review panel** (`wuffagent-egui/src/ui/improvements/`): per-field approve toggles, prompt
   editing (F1), append-dedup (F2), revert-to-snapshot (F4), rejection lesson on dismiss (F5),
   applied-prompt marker on approve (I5).
6. **Self-tools**: `list_agents` / `edit_agent_profile` (history snapshots), `restart`
   (build_cmd + test_cmd + `.prev` backup + resume marker), `handoff` / `hand_back` (in-turn +
   sub-session), MCP management (7 tools), plugin self-extend (`reload_plugins` /
   `add_plugin_path`, T3b), shell (allowlist + dangerous-command filter), skills, memory.
7. **Verification loop (S1)**: judge LLM + nudge retry; terminal outcomes feed metrics.

## Gaps (findings, with evidence)

### G1. Effect check is blind to the best data (HIGH)
`improver.rs::effect_check_section` (L154-169) compares only **lesson entries** since the
`improvement-applied` marker. The metrics M1 built — run outcomes, tool-error rate, feedback
up/down — are the strongest before/after signals and are **not used** by the effect check
(`suggest_improvements` feeds them to the LLM as "recent 7 days", but the effect check has no
windowing around the marker). Additionally:
- **Field-only approvals record no marker at all**: `draw.rs:356` calls
  `remember_applied_prompt` only when `prompt_applied` is true (set only for prompt writes,
  `improvements/memory.rs:134`), so tools/shell/reasoning/handoff/timeout changes are never
  effect-checked.
- The effect check lists lessons but never the **number of runs since the change** (the
  denominator that makes the comparison meaningful) — `runs_since` (improver.rs:113-124)
  already exists and is unused here.

### G2. No on-demand or cross-agent self-review (MEDIUM)
The check fires only inside `post_task_maintenance` (engine.rs) for **the agent that just
finished a task**, gated by a **process-global** task counter (`improvement_due`,
engine.rs:222-237: `tasks_completed` is one shared counter). Consequences:
- the user cannot say "review now" — they must wait for cooldown + new evidence;
- one busy agent consumes the cooldown for everyone; a single idle agent never gets checked
  while another burns tasks;
- cross-agent patterns (handoff chains, shared tool gaps) are never analyzed — each check sees
  exactly one agent's data.

### G3. Self-improvement health is invisible (MEDIUM)
`ImprovementsPanel` shows only pending suggestions. The user cannot see when the last check
ran, what it concluded ("no change needed" vs "2 suggestions" vs LLM error), or why the gate
blocked a check (cooldown remaining / no new evidence). `ImprovementState` (memory/manager.rs)
persists only `last_check: Option<i64>`. A silent no-op loop looks like a broken one.

### G4. Pending suggestions are lost on app exit (MEDIUM)
`ImprovementsPanel.pending` is in-memory only (improvements/mod.rs:113-117). Approve/dismiss
are the only exits; closing the app drops unreviewed suggestions. Re-suggestion is not
guaranteed (the new-evidence gate re-arms, but the LLM may produce different wording, so the
F2 agent+rationale dedup may not catch it).

### G5. The agent cannot read its own metrics (MEDIUM)
No tool exposes `MetricsLog` to the model. The `wuffagent` profile (the one whose job is to
improve itself) must `shell` into `~/.wuffagent/metrics/*.jsonl` to judge its own runs. There
is no "how is coder doing?" answer in chat.

### G6. Skills have no lifecycle (LOW-MEDIUM)
Skills are saved + injected but: no usage is recorded (a skill that is never `read_skill`ed is
indistinguishable from a hot one), no maintenance pass (memory has one), and the improver has
no skill signal in its evidence. Cheap first step: a `skill_use` metric line; maintenance is
stretch.

### G7. Metrics lack cost + task identity (LOW, stretch)
`RunStats` (agents/types.rs:69) is tool_calls/tool_errors/verification_attempts only — no
prompt/completion tokens (the `usage_recorder` telemetry exists but is not correlated per
agent run) and no session/task id linking a run to its feedback line or an improvement marker.
Prompt bloat (the most likely side effect of approved prompt changes) is therefore not visible
in the effect check's data.

### G8. Small improver/apply gaps (LOW)
- **a.** `ImprovementSuggestion` has no `description` field — the profile description drives
  agent selection and handoff targeting, and the improver (which sees the agent at work) is
  the best position to rewrite it.
- **b.** Cooldown is process-global (see G2) — per-agent counters are the fix.
- **c.** Effect check lacks "N runs since change" (see G1).
- **d.** The marker stores the applied date as a **string in the content** (improvements/
  memory.rs:243-252); a machine-readable timestamp is needed for windowed before/after
  comparisons (G1). (Check whether `MemoryEntry` already carries a created-at timestamp the
  effect check can use; if yes, no content change needed — only a read path.)

### G9. No eval / regression harness for prompts (STRETCH)
"Did the prompt change help?" is currently judged from post-hoc metrics + user thumbs. A
fixed per-agent task set with scored runs before/after a change would make prompt changes
objective and gate auto-apply. Big; design separately.

## Phases

### Phase 1 — close the effect-check loop (G1 + G8a + G8c + G8d) — core, ~1 day

1. **1a. Windowed metric summaries.** `agents/metrics/`: generalize the existing
   `summary_since(Option<DateTime<Utc>>)` into
   `summary_between(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) ->
   MetricsSummary` (keep `summary_since` as a thin wrapper so the UI/improver call sites are
   untouched). Also `runs_since(since)` is already there (improver.rs) — move/keep as is.
2. **1b. Marker for ALL approved profile changes.** Generalize `applied_marker`
   (improvements/memory.rs:242): content names the applied fields ("prompt, allowed_tools")
   and the date; keep the `improvement-applied` + `agent:<name>` tags (the tag is what
   `latest_applied_marker` finds — content wording is free to change). `apply_improvement_
   detailed` already computes the `applied: Vec<String>` list (memory.rs:100-129) — return it
   (extend the tuple or a small struct) so `draw.rs:356` records a marker whenever a profile
   write succeeded, not only for prompts. Prefer a machine-readable timestamp: if
   `MemoryEntry` exposes a created-at field, the effect check uses it; otherwise keep the
   date string and parse it (content already has `%Y-%m-%d`).
3. **1c. Effect-check section v2.** `improver.rs::effect_check_section` becomes a
   before/after comparison using 1a: window BEFORE = 7 days ending at the marker ts, window
   AFTER = marker ts → now; render both via `MetricsSummary::format_line` (rename the
   hard-coded "Recent metrics" prefix to a parameter so one rendering serves both windows) +
   "N runs since the change" via `runs_since`. Keep the existing lesson list as third item.
   Update the "Recent metrics" extraction prompt text (improver.rs:331) only if wording
   collides — it stays 7-day-based.
4. **1d. Propose `description`.** `ImprovementSuggestion.description: Option<String>`
   (serde default — old JSON still parses), improver prompt field list + JSON template
   (improver.rs:264-287), `PendingImprovement.description` + `apply_description` toggle
   (default true), apply path in `improvements/memory.rs` (`config.description = …`, add
   "description" to the applied list), serialization + suggest tests extended (the I2 tests in
   `improvement/tests/` are the pattern).
5. **1e. Per-agent cooldown (G8b).** `engine.rs::improvement_due`: replace the shared
   `tasks_completed` gate with a per-agent map (`HashMap<String, usize>`: last task-counter
   value at which THAT agent was checked); a check is due when the agent's own task count
   advanced ≥ `improvement_cooldown_tasks` since its last check. Keep the new-evidence gate
   as is. (Small, same file, ships with the loop work.)

**Tests (1):** windowed summary (before/after/empty), marker content for field-only approval,
effect-check section contains both windows + run count (fake marker + seeded temp metrics dir,
`MetricsDirGuard` pattern), description round-trip through suggest + apply, per-agent due
logic (two agents, one busy).
**Gate:** `cargo test --workspace` green, no warnings, one commit per sub-item, self-restart
(build-gated) after core changes, update memory.

### Phase 2 — visible + durable loop (G3 + G4 + G2a) — core + egui, ~1 day

1. **2a. Last-check outcome persisted.** Extend `ImprovementState` (memory/manager.rs:524)
   with `last_agent: Option<String>` and `last_result: Option<String>` ("no change needed" /
   "N suggestion(s)" / "error: …"); `record_improvement_check(result)` signature change (two
   call sites in engine.rs); getter `last_improvement_check() -> Option<(i64, String,
   String)>`.
2. **2b. Panel status header.** `improvements/draw.rs` (or a new small
   `improvements/status.rs` to keep draw modular): one line above the pending list —
   "Last check: <relative time> on '<agent>' — <result>; next auto-check: <n> task(s) away,
   evidence: <new runs since check / none>". Engine already exposes the pieces (2a + the
   metrics count used by the evidence gate).
3. **2c. "Check now" button.** `AgentEngine::check_improvements_now(&self, agent_name)`:
   factor the body of the post-task check (engine.rs:166-212) into a private
   `run_improvement_check(agent, force: bool)`; the public method calls it with `force=true`
   (skips cooldown + evidence gate), the post-task path keeps `force=false`. The panel button
   calls it via `ChatApp` (engine is already owned there); suggestions flow through the
   existing `AppEvent::ImprovementSuggested` path unchanged.
4. **2d. Pending suggestions survive restart.** New core module
   `agents/improvement/pending.rs`: `PendingStore` at `<wuffagent_home>/pending_improvements.
   json` — `Vec<ImprovementSuggestion>`, atomic write (existing atomic-write helper pattern
   from the MCP config), tolerant load (corrupt file → empty + warn), `set_metrics_dir_
   for_testing`-style override. egui: `ImprovementsPanel` loads on init (ChatApp bootstrap),
   rewrites on add/refresh/approve/dismiss. The F2 dedup rule is reused so a restart-restore
   never stacks duplicates.

**Tests (2):** state last_result round-trip; pending store round-trip + corrupt file +
restore-dedup; `check_improvements_now` runs the check with a fake LLM (engine test) while
`force=false` is still gated.
**Gate:** as Phase 1.

### Phase 3 — self-data the agent can read (G5 + G6-lite) — core, ~½ day

1. **3a. `read_metrics` builtin tool.** New `tools/builtin/metrics.rs` (one small Tool struct,
   the `skills.rs`/`agent_profile.rs` pattern): params `{ agent: Option<String>,
   limit: usize (default 20, max 100) }`. Output: for the requested agent (or, when omitted,
   every agent file in the metrics dir with lines) — the 7-day `MetricsSummary::format_line`
   + the newest `limit` lines via `describe()`. `MetricsLog` construction is side-effect free,
   so the tool binds `Arc<MetricsLog::default()>`; register via
   `register_metrics_tools(&registry)` in the main.rs chain (next to the skill tools) and add
   `read_metrics` to the `wuffagent` profile's `allowed_tools`.
2. **3b. Skill-use metric lines.** Extend `MetricsLine` with
   `SkillUse { ts: DateTime<Utc>, skill: String }` (serde tag `skill_use` — additive, old
   lines still parse), `describe()` renders it, `log_skill_use(agent, skill)` helper;
   `read_skill` writes one line on success (agent name from the per-run config — if the skill
   tools don't already receive the agent identity, thread it the same way the memory tools do;
   otherwise default agent name is acceptable). MetricsSummary unchanged (skill lines are
   visible in `recent`/the improver's recent lines, which already render via `describe()`).

**Tests (3):** read_metrics round-trip (temp metrics dir: seeded run+feedback lines → expected
text), unknown agent → "no data", limit clamping; skill-use append + tolerant read of the new
line kind by old code paths.
**Gate:** as Phase 1.

### Phase 4 — stretch / deferred (design before implementing)

- **G7**: tokens + task identity in the run line (prompt/completion tokens from the client
  usage path, session id) → prompt-bloat visibility in the effect check.
- **G2b**: cross-agent periodic review (app-start or idle-triggered; collects every agent's
  recent metrics + handoff hops; the improver gets a multi-agent evidence mode).
- **G9**: prompt eval harness (per-agent fixed task set, scored runs before/after; could
  auto-gate future auto-apply).
- **G6-full**: skill maintenance pass (usage-driven prune/merge, modeled on memory
  maintenance).
- **Profile drift note** (observed 2026-10-06): the project `agents/` dir (a search dir)
  carried a drifted `wuffagent.json` while the canonical config-dir copy
  (`C:\Users\troop\.wuffagent\agents\`) is the active one. One-line AGENTS.md note: config
  dir is canonical; project `agents/` only adds/overrides.

## Order & rationale

P1 → P2 → P3. P1 makes the loop's conclusions *trustworthy* (it is already running — feeding
it its own data is the highest-value fix). P2 makes it *trustable by the user* (visible +
durable + on demand). P3 closes the agent's own feedback gap (it can read what it is judged
on). P4 only after P1–P3 prove out.

## Housekeeping (this round, done or to do)

- ✅ `t3b-runtime-plugin-reload.md` marked DONE (reload_plugins + add_plugin_path exist in
  `tools/builtin/plugins.rs`, are in the active wuffagent profile's `allowed_tools`, and were
  exercised in-session 2026-10-06).
- ⚠ Suspected flake in the `search_content` builtin (2026-10-06, M: drive): wide-dir searches
  (`wuffagent-core/src`, 194 files; `ui/improvements`) returned 0 matches for patterns that
  demonstrably exist, while narrower paths work. Candidate bug: check the walk/size limits in
  `tools/builtin/search.rs` when next touched. (Workaround: retry narrower or `Select-String`.)
- ⚠ The project `agents/wuffagent.json` disappeared from disk mid-session (untracked in git,
  git status clean) — consistent with the drift note above; no code path in WuffAgent deletes
  agent files, so external (user/editor) deletion is the likely cause.

## Out of scope (deliberately)

- Auto-apply of improvements (human review stays the gate; G9 would be its prerequisite).
- Sandboxing plugin loads (T3b decision stands).
- Any change to the verification nudge budget / judge prompt (S1 territory).
