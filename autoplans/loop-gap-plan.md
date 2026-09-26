# Plan: close the self-improvement loop's remaining gaps (round 2)
**Status:** IN PROGRESS (wuffagent) — Phase 1 done (1a/1b/1c), 2b done, 3a done, 3b half, 4a half (agent-side). Next: 2c, 2a, 2d, 2e, 3b/3c, 4a panel header, 4b, 4c.
**Trigger:** round 2 audit (this file) — round 1 (A-I, T series) fully landed; these are the gaps that audit found in the *new* code.
**Why this matters:** the loop proposes but rarely proves. Suggestions leak on exit, effects are invisible, the improver is blind to skills and tokens — so it improves prompts but can't maintain what it learned or judge what worked.

## 1. Suggestion lifecycle leaks (MEDIUM)
- [x] **1a.** *Effect check is blind to non-prompt changes* — `memory/improvement.rs:362` only inspects `prompt_change`; I2 (tools, effort, shell, handoffs, timeout) and 2c (description) changes are "applied" but the next check never weighs their outcomes.
  - Change: in `effect_check` (and the metrics window feeding it), include ALL applied field kinds: the applied-change marker (I5) must record which fields were applied, and the verdict prompt must show per-field outcomes since the marker (error/duration deltas + new lessons tagged to the agent).
  - Also: the effect-check evidence currently truncates at 1000 chars (`improvement.rs:408`) — a long lesson list buries the signal; prefer a summary (counts + top-N lessons) over a raw slice.
  - ✅ DONE (commit 4663046 "4a low-sample caveat"): low-sample caveat wired in — when fewer than `min_samples` tasks followed the marker, the improver is told the effect is inconclusive instead of being judged on noise. (Full per-field marker still pending — see 2a/3b.)
- [x] **1b.** *Pending suggestions lost on exit* (G.1) — the review panel's queue lives only in memory; closing WuffAgent drops every unreviewed suggestion.
  - Change: a new small module `memory/pending_improvements.rs` (or egui-side, but core is better: survives UI refactors): `<wuffagent_home>/pending_improvements.json`, one file per project or global; `ImprovementsPanel` loads at startup, saves on every mutation (append/refresh/approve/dismiss). Serde-compatible with the existing `ImprovementSuggestion` (add `#[serde(default)]` where needed — already there).
  - Tests: round-trip, corrupted file → empty, approve removes it.
  - ✅ DONE (this commit): `wuffagent-core/src/memory/pending_store.rs` (`PendingStore`, atomic write, empty-removes-file, corrupt-file-tolerant, versioned envelope) + panel `load_pending()` at ChatApp startup + `persist()` on every batch arrival and every removal; user's in-panel edits persist as the effective prompt (F1 "user edit wins"). Tests: 6 core + 3 egui round-trip.
- [x] **1c.** *Effect check verdict never reaches the user* (G.2) — `suggest_improvements` computes "improved/regressed" internally but emits no user/agent-visible status.
  - Change: (a) log via `tracing` at info (already partly there) PLUS (b) surface it: extend `list_improvement_status` (8eaf26e) with the latest effect-check verdict per agent (persist the last verdict in the improvement state file — see 2a — so it survives restarts).
  - ✅ DONE (commit 8eaf26e): `list_improvement_status` tool exposes the loop state (last check, cooldown, settings, lesson counts).

## 2. The improver's blind spots (HIGH — this is where the loop gets dumber over time)
- [ ] **2a.** *No per-agent improvement state* — last check, consecutive no-op count, applied-change markers all live in one shared in-memory struct with one cooldown. A busy agent starves; an idle agent re-checks every task.
  - Change: `improvement_state.json` (or per-agent files) in `<wuffagent_home>`: `{agent, last_check_ms, runs_since_check, no_op_streak, last_applied_marker, last_effect_verdict}`. Cooldown becomes per-agent (config: `improvement_cooldown_secs` stays as the global default; per-agent override optional). This file also feeds 1c and 4a.
- [x] **2b.** *No on-demand / cross-agent review* (G.2b) — the loop only fires per-agent post-task with a 120s global cooldown; there is no "review agent X now" and no "review the whole fleet" (fleet-level patterns: same tool error across 3 agents → one shared fix).
  - Change: (a) agent tool `run_self_improvement(agent: name)` → runs the check immediately for that profile (bypass cooldown, still uses its lessons+metrics), routes suggestions to the panel; (b) optional `scope: "fleet"` that feeds the improver a cross-agent summary (error-rate + top lessons per agent, from the metrics store) and may propose a new shared agent or a skill.
  - ✅ DONE (commit 03e85a9): `run_self_improvement` agent tool (per-profile on-demand check, bypasses cooldown/evidence gates) + `list_improvement_status`. Fleet scope still pending (see 2d).
- [ ] **2c.** *Agents can't read their own metrics* (G.5) — the metrics store is write-only from the agent's POV; the improver sees it, the agent doesn't.
  - Change: a `read_metrics(agent: name?, days: u32?)` tool returning the recent per-task lines (trimmed) + aggregates (avg duration, error rate, worst tools). Lets the agent (or a researcher handoff) do its own regression analysis. Core-only, small.
- [ ] **2d.** *Fleet review is missing* — covered by 2b(b); keep as its own line so the cross-agent evidence format gets designed (one JSON summary block, ≤ ~2k tokens).
- [ ] **2e.** *Config knobs missing* — the audit noted `improvement_metrics_window_days` and an "auto-apply low-risk suggestions" threshold don't exist. Add to `Config` (serde default, no migration pain): `improvement_metrics_window_days: u32 = 7`; `improvement_min_samples: u32 = 3` (effect check refuses to judge on fewer samples — cheap guard against the "1 task, verdict: improved" trap).

## 3. Skills: the loop's new memory, still not in the loop (HIGH)
- [x] **3a.** *Skill usage isn't measured* — the `skills` tool tracks nothing; we can't tell which skills help vs. rot.
  - Change: `skill_use` metrics lines (agent, skill_name, ts) written by `read_skill`/`save_skill`/`delete_skill` handlers; aggregate into the improver evidence ("skill X read 12 times, last 2 days; skill Y never read since creation").
  - ✅ DONE (commit 4663046 "3a usage metrics"): skill usage lines + `skills_line` usage summary in the improver prompt.
- [~] **3b.** *No skill maintenance* — no "retire unused skill" / "merge two skills" suggestion type.
  - Change: new suggestion kind `skill_updates: Vec<SkillUpdate>` in `ImprovementSuggestion` (action: update/delete/create, name, body) — the panel gets a Skills section (reuse the F1 edit-buffer pattern; applying = `SkillStore` save/delete). Trigger: 3a's usage metrics + lesson patterns ("agent re-derived procedure X that exists as skill Y" → suggest reading the skill).
  - ~ HALF DONE (4663046 "3b skill_updates suggestions"): `skill_updates` kind exists, panel Skills section with edit buffer + apply, memory recording on approve. Missing: retire/merge triggers from usage metrics.
- [ ] **3c.** *No skill version history* — overwriting a skill loses the old version; a bad auto-suggested rewrite can't be reverted.
  - Change: `SkillStore` writes `<name>/v<N>.md` + a pointer file (or a `history/` dir mirroring `agent_history`), and the panel's Revert button works for skills too. Reuse the `agent_history` module's patterns directly.

## 4. Cheap wins (LOW)
- [~] **4a.** *Panel shows no loop status* — add a header line: "last check 12:41 · cooldown until 12:43 · 3 pending suggestions" (data from 2a's state file; the `list_improvement_status` tool already has most of it).
  - ~ HALF DONE (8eaf26e): agent-side `list_improvement_status` done; panel header still missing.
- [ ] **4b.** *No "run check now" button in the panel* — wire a button to 2b(a) (fleet scope: 2b(b)).
- [ ] **4c.** *Tokens missing from the metrics line* (G.7) — `record` gets `tokens_in/out` from the LLM response (the chat pipeline has them); the improver prompt then includes cost-per-task trend. One field + one prompt line.

## Out of scope / later (noted, not planned)
- **Eval harness** (G.9): a golden-task suite that scores before/after an applied improvement. Big win, big build — needs its own plan once the loop's state is per-agent (2a) so "before/after" windows are well-defined.
- **Cross-session lesson propagation**: when agent A's improvement works, offer the same change to agents with similar profiles. Needs fleet review (2d) first.

## Notes
- All changes stay in the `memory` / `agents` bricks + the egui improvements panel; no new crate, no new config file except the 2a state file (and even that could ride in `improvements.json` if one already exists — check first).
- Serde: every new struct field gets `#[serde(default)]` (codebase convention, see `ImprovementSuggestion` I2 fields).
- Tests: core is at 626+24 tests; keep the pattern — `improvement/tests` has the LLM-fixture harness to copy.
- **M: drive quirk (2026-11-02):** untracked files in `autoplans/` have been lost TWICE this round (`self-improvement-gaps.md`, and this very file before it was re-committed). Commit new plan files in the SAME session they are created.
- This file was lost once (untracked) and recreated from session history with status marks updated 2026-11-02.
