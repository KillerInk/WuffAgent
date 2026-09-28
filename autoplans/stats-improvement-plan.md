# WuffAgent Statistics Improvement Plan (2026-09-29)

**Status:** OPEN (wuffagent). Companion to `self-improvement-gaps-3.md` (the
improvement *loop* over these metrics — this plan is the metrics/statistics
side: what is recorded, what can be computed, and what the user/agents can
see).

## Current state (verified in code, 2026-09-29)

Two parallel, unjoined JSONL stores, both under `~/.wuffagent/`:

**1. `usage.jsonl` — one line per LLM call** (`wuffagent-core/src/usage/recorder.rs`)
Fields: `ts, session_id, agent, model, prompt/completion/total_tokens,
tool_calls (count), thinking_chars`.
- Analysis: `usage/stats.rs` — tolerant load, hour/day/week zero-filled
  bucketing (wall-clock, DST-safe), incremental `UsageLogReader` (byte-offset
  polling, torn-line buffering).
- Reader: `wuffagent-egui/src/ui/usage_panel.rs` — token bar chart with
  fixed 24 h / 30 d / 12 w ranges, stat cards, hover details.

**2. `metrics/<agent>.jsonl` — one line per event** (`wuffagent-core/src/agents/metrics.rs`)
Line kinds: `run` (tool_calls, tool_errors, verification_attempts,
duration_ms, outcome, tokens_in/out), `feedback` (message-level 👍/👎),
`skill_use` (fleet `skills.jsonl`), `trim` (context-rot signal), `check`
(loop cost), `eval` (id, passed, score?, duration, tokens).
- Analysis: `MetricsSummary` (windowed counts + sums only), `lines_since`,
  `skill_usage_since`, `loop_cost_since`. **Every read is a full-file scan
  (`read_all`); there is no incremental reader and no rollups.**
- Readers: `read_metrics` tool (window aggregates + 10 newest raw lines,
  fleet one-liners, `status=true` loop-status join), `list_improvement_status`,
  the improver prompt ("Recent metrics"), the agent editor block
  (`agent_config.rs:521` — **all-time counts + last 5 lines**, and it calls
  `summary_since(None)` + `recent(5)` = **two full file scans per redraw**).

**What the user/agents can actually answer today:**
- Total tokens/calls/tool-calls per hour/day/week (fleet-wide only — the
  usage panel has **no agent or model filter** even though the data is there).
- Per-agent windowed counts: runs, outcome breakdown, error rate, avg/max
  duration, token totals, feedback up/down, trim count, check cost, eval
  pass count.
- The improvement loop's per-agent state (checks, verdicts, no-op streaks).

That is the "basic" part: **flat counts and means, no breakdowns, no
trends, no costs, no joins.**

## Gaps (ordered by impact)

### A. Missing data — things no reader can compute because no writer records them
- **A1. No per-tool attribution (HIGH).** Run lines carry only aggregate
  `tool_calls`/`tool_errors`. "Which tool is most error-prone for coder?" is
  unanswerable — the `read_metrics` module doc admits it ("'worst tool' is
  not available"). No per-tool duration either (a stuck shell command and a
  stuck LLM round are indistinguishable in duration).
- **A2. No run identity / no cross-store join (HIGH).** Metrics lines have no
  `run_id` and no `session_id`; usage lines have `session_id` but no run
  link. You cannot reconstruct *which LLM rounds happened inside a failed
  run*, correlate a `gave_up` with its token profile, or attach per-run
  user feedback (gaps-3 item 3c `feedback{run_ts,…}` was never built —
  `run_ts` appears nowhere in the codebase).
- **A3. No model on run lines (MEDIUM).** The usage store records `model`
  per call, but `MetricsLine::Run` doesn't, so outcome/error-rate-per-model
  (the metric that would answer "is model X worse for this agent?") needs a
  join that A2 says doesn't exist.
- **A4. Eval score is never written (MEDIUM).** `MetricsLine::Eval` has
  `score: Option<f64>` (documented as "2d: LLM-graded") but `log_eval`
  hard-codes `score: None` — the eval harness records pass/fail only. No
  quality trend, only a binary trend.
- **A5. No cost (MEDIUM).** Tokens are counted everywhere but there is no
  price table anywhere in `config/` (grepped: no `price|cost_per|usd` in
  core). "What did last week cost?" is unanswerable in every view.
- **A6. No duration breakdown (LOW–MEDIUM).** One `duration_ms` per run;
  LLM time vs tool time vs verification time are mixed together, so a slow
  run can't be triaged from the log.
- **A7. Per-run user feedback (LOW–MEDIUM).** Feedback is message-level only
  (gaps-3 3c, promoted here): `feedback {ts, run_id, up/down}` would give
  task-level sentiment and let "👎 → what did that run do?" be answered.

### B. Missing analysis — data exists but no reader computes it
- **B1. No distribution stats (MEDIUM).** Only mean + max for duration.
  No p50/p95 for duration or per-run tokens — with a handful of 40-minute
  runs, the mean is misleading.
- **B2. No trends (MEDIUM).** The metrics store has no bucketing at all;
  the usage store's `bucketize` is not applied to runs/outcomes/error rate.
  "Is the error rate going up?" needs a daily/weekly series, not a window
  count.
- **B3. No comparison windows (MEDIUM).** `read_metrics` gives one window.
  The improvement loop does before/after around I5 markers internally, but
  the agent-facing tool cannot say "error rate 7d vs previous 7d" — the
  comparison the loop's evidence gate (1a) computes is not exposed.
- **B4. No per-model breakdown in the UI (MEDIUM).** `model` is in
  `usage.jsonl`; nothing aggregates by it.
- **B5. No outcome correlations (LOW).** e.g. "runs with ≥1 trim → gave_up
  share vs runs without", "retry rate trend". Cheap once A1/A2 land.

### C. Missing UI — the user-facing surface
- **C1. No fleet dashboard (HIGH).** The only fleet view is the
  `read_metrics`/`list_improvement_status` tool text. There is no panel with
  per-agent KPI cards + 30-day trend charts (runs, error rate, outcome mix,
  tokens/cost). The loop-status join (groundwork, gaps-3) exists in tool
  form only.
- **C2. Agent editor metrics are all-time-only and unfiltered (MEDIUM).**
  `summary_since(name, None)` + last 5 lines; no 7d/30d toggle, no per-tool
  table, no outcome chart (data for all of it exists after A1).
- **C3. Usage panel has no filters and no cost (MEDIUM).** Agent and model
  are recorded per line but unfilterable; token counts have no cost card
  (A5 prerequisite).

### D. Engineering — the "basic approach" underneath
- **D1. Full-file re-scan on every read (HIGH, cheap).** No incremental
  reader for the metrics store (the usage store has `UsageLogReader`);
  `read_all` re-reads + re-parses the whole file per call, and the agent
  editor does that **twice per redraw frame**. Fine at 100 lines, painful
  at 100k.
- **D2. Unbounded logs (MEDIUM).** Both stores grow forever; no rotation, no
  daily rollup, no pruning. A long-lived install will hit MBs of JSONL being
  re-parsed per query.
- **D3. Two stores, two formats, no shared schema discipline (LOW).** No
  schema version field; join keys missing (A2). The two `MetricsLine`
  timestamp match arms and the two `line_ts` helpers (metrics.rs +
  read_metrics tool) already duplicate.
- **D4. No export (LOW).** No way to get windowed CSV/JSON out for external
  analysis or a bug report.

### E. Agent-facing tooling
- **E1. `read_metrics` stays flat (MEDIUM).** No per-tool table (after A1),
  no percentiles (B1), no `compare` mode (B3). The agent's regression
  analysis is limited to reading 10 raw lines.
- **E2. Evidence-gate deltas not surfaced (LOW).** 1a computes
  error-rate/gave_up deltas to *re-arm* the check, but the status views
  show the gate as "new evidence: yes" without the numbers (partially done
  for lesson-driven; the metric-delta line should always render when that
  path fired).

## Plan

### Phase 1 — record what's missing (writers only; all serde-tolerant; ~1 day)
- **1a. Per-tool histogram on run lines (A1).** New field
  `tools: Vec<ToolStat { name, calls, errors, duration_ms }>` (or `#[serde(
  default)]` map) on `MetricsLine::Run`, filled by the same
  `&mut RunStats` path that fixed commit 1827079 (per-tool increments in
  `run_native_tool_calls`/`run_text_embedded_calls` — the call site already
  knows the tool name and can time each call). Cap the vec (e.g. top-32 by
  calls, rest folded into `"__other__"`) so a 1000-tool run stays one
  compact line. `MetricsSummary` gains `tool_stats` accumulation;
  `describe()` unchanged (the UI/tool reads the field).
- **1b. Model + cost on run lines (A3, A5).** Add `model: String` (from the
  server-reported model of the run's calls; `""` when unknown) and
  `cost_usd: f64` to `Run` (and `Check`/`Eval`): a small price table in
  `config/` (`model_prices: Vec<(model, per_1M_in, per_1M_out)>`,
  serde-defaulted, user-editable, fallback 0.0 = "recorded but unpriced").
  Cost is computed at write time (stable history even if prices change).
- **1c. Eval score (A4).** The `score` field exists on `MetricsLine::Eval`
  but `log_eval` hard-codes `None` — and the verification judge
  (`verify_tool_outputs`, verified in `run_eval.rs`) returns
  `verified: bool` + `judge_reason: String` with **no score today**. So this
  needs a real change, not a wire-up: extend the judge prompt (the `expect`
  grading call in `run_eval`) to also emit a `0.0–1.0` score line and parse
  it into the verdict struct (fallback: 1.0 pass / 0.0 fail, labeled
  "derived"). One extra line in the prompt, not a second LLM call.
- **1d. Duration split (A6).** `Run` gains `llm_ms` and `tools_ms`
  (verification LLM time folds into `llm_ms` or a third field
  `verify_ms` — pick during impl, keep `duration_ms` as the total for
  backward compat).
- **1e. run_id + session_id (A2, the join key).** `Run` (and `Trim`, and the
  new per-run feedback) gain `run_id: String` (uuid or
  `ts-agent-counter`); `session_id: String` where the run has one. The usage
  recorder gets the same `run_id` (it's called per LLM round inside the
  loop — thread the id through). This is the enabler for B4/A7; cheap now,
  expensive to backfill later.
- Gate: `cargo test --workspace` green; no warnings; one commit per item;
  restart (core changed). All new fields `#[serde(default)]` (established
  pattern — old lines stay readable).

### Phase 2 — compute what's new + make reads cheap (~1 day)
- **2a. Incremental metrics reader (D1).** Port the `UsageLogReader` pattern
  (byte offset, torn-line buffer, shrink-rescan) to `MetricsLog` as
  `MetricsLogReader`; cache parsed lines in the egui app keyed by
  (file, offset) so the agent editor stops doing two full scans per frame.
  Keep `read_all` for the tool path (one-shot) — or route it through the
  reader too.
- **2b. Trend bucketing for the metrics store (B2).** Reuse
  `usage::stats::bucketize`'s `Granularity` (hour/day/week) — extract the
  shared bucket math into a small `stats_core` (or make `usage::stats`
  generic over a `Fold` trait) so runs/outcomes/error rate/tokens get the
  same zero-filled windows. This kills the D3 duplication too.
- **2c. Distribution + comparison primitives (B1, B3).** `MetricsSummary`
  (or a new `MetricsReport`) gains p50/p95 duration+tokens and a
  `compare_with_previous_window()` (same-length preceding window — the exact
  before/after rule the I5 effect check uses, so the numbers match the
  loop).
- **2d. `read_metrics` v2 (E1, E2).** Add a per-tool table (from 1a:
  top-5 by errors, with call counts + error % + avg ms), p50/p95 lines, and
  a `compare: bool` param (renders current window vs previous window with
  deltas — "error rate 12% → 34% (Δ+22pp)"). Surface the 1a evidence-gate
  deltas verbatim in `list_improvement_status` when that path fired (E2).
- Gate: workspace green; `read_metrics` on the live store returns the new
  sections (dogfood: call it as an agent); commit per item.

### Phase 3 — the user-facing surface (~2 days)
- **3a. Fleet dashboard panel (C1).** New top-level egui panel: per-agent KPI
  cards (runs, outcome %, error rate, tokens+cost, last activity — all
  windowed, default 30d) + a trend chart reusing the usage panel's chart
  code (dual-series: runs bar + error-rate line). The loop-status section
  (from `read_metrics status=true`'s data, computed in core) renders as a
  compact table. Data comes through the Phase-2 primitives, so the panel is
  render-only.
- **3b. Agent editor windowing (C2).** Replace the all-time block: 7d/30d/
  all-time toggle, per-tool table (1a data), outcome mini-chart, and — once
  A7 exists — per-run feedback. Backed by the cached reader (2a), so the
  per-frame cost is the incremental tail only.
- **3c. Usage panel filters + cost (C3).** Agent and model dropdown filters
  (the fields exist per line) and a cost stat card (1b price table; "unpriced
  tokens" noted when `cost_usd == 0`).
- **3d. Per-run user feedback (A7, the 3c of gaps-3, done here because the
  UI is already open).** 👍/👎 on the last N run lines in the agent editor →
  `MetricsLine::Feedback { ts, run_id, … }` (serde-tolerant: legacy lines
  have no `run_id` and still count as message-level). Summary counts them
  separately (run-level sentiment vs message-level).
- Gate: workspace green; restart; screenshot the dashboard on the live
  store; commit per item.

### Phase 4 — joins, lifecycle, export (~1 day)
- **4a. Cross-store join views (B4, B5).** With `run_id` in both stores
  (1e): "run detail" — given a run line, list its LLM rounds (model,
  tokens, thinking_chars per round) and its per-tool stats; and the cheap
  correlations (trim→gave_up share, retry trend) computed in core and shown
  in the agent editor / `read_metrics`.
- **4b. Rollup + rotation (D2).** Nightly (first app start after midnight,
  best-effort) write `metrics/rollups/<agent>-YYYY-MM-DD.json` (one
  day-summary per agent) and prune raw `run` lines older than
  `metrics_retention_days` (new config, default 90; feedback/eval/trim/check
  lines kept — they're the small, high-signal kinds). Readers check the
  rollup for out-of-window history so pruning never loses aggregates.
- **4c. Export (D4).** `read_metrics` gains `export: "csv"|"json"` (windowed
  raw lines → temp file path in the output) — or a dedicated
  `export_metrics` tool if the param sprawl bites; either way one file the
  user can open for external analysis.
- **4d. Schema version (D3).** `v: u32` (default 1) on all line kinds; the
  tolerant-parse behavior is unchanged, the version is just the migration
  anchor.
- Gate: workspace green; restart; verify a day of live lines rolls up and
  prunes correctly on a copy of the store (test override dir — never the
  real one, per the metrics-pollution lesson).

## Explicit out of scope
- Sentiment/quality analysis of actual task content (needs the session
  store, own plan).
- Changing the improvement *loop* itself (gaps-3 territory; this plan only
  feeds it better data — 1a/1e make its 1a evidence gate strictly more
  precise, which is a free win, not a change).
- Multi-user / remote aggregation (single-machine app).
- GPU/egui perf work beyond the reader cache (2a).

## Order & gates
Phase 1 → 2 → 3 → 4 (2 needs 1's fields; 3 needs 2's primitives; 4 needs 1e).
Each phase: `cargo test -p wuffagent-core` + `-p wuffagent-egui` + workspace
build green, no warnings, one commit per item, restart WuffAgent after core
changes (dual-target self-restart; verify which build is live afterwards).
Commit this plan immediately after writing (M: drive drops untracked files).

## Verification targets (definition of done for the round)
- `read_metrics` answers "which tool fails most for agent X, and did it get
  worse this week?" — per-tool table + comparison window, from real data.
- The fleet dashboard renders per-agent KPIs + 30-day trends; the agent
  editor shows windowed stats without a full-file scan per frame (measurable
  in a release build or by code inspection of the cached reader).
- Token spend shows a USD figure wherever it shows tokens (priced models),
  and eval lines carry real scores.
- A `gave_up` run can be traced to its individual LLM rounds via `run_id`.
- 90+ day old raw run lines are rolled up + pruned without losing any
  aggregate the dashboards or the loop read.

## Notes / hazards
- **Metrics pollution:** all writer changes get tests under
  `set_metrics_dir_for_testing` (process-global lock, like `CONFIG_PATH_LOCK`
  — the lesson from the 1827079 era). The run-writer lives in the hot path;
  the per-tool histogram must stay a cheap increment (no allocation per
  call — reuse the existing `RunStats` slot).
- **Back-compat:** every field added in Phase 1 must be `#[serde(default)]`
  and every reader must tolerate lines missing it (the 4c/1c/2b pattern
  already in `metrics.rs`). Old lines get `model=""`, `cost_usd=0.0`,
  empty tool lists — summaries must treat those as "unknown", not zero
  (a 50% unknown-model share is a real signal, not noise to drop).
- **Cost table cold start:** ship with an empty default table (cost = 0 =
  unpriced) rather than guessing prices; the user adds their provider's
  rates. Wrong hardcoded prices would mislead more than no price.
- **M: drive:** mtimes lie, untracked files vanish — commit each phase's
  artifacts; verify file existence with shell, not `list_dir` freshness.
