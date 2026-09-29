# WuffAgent Statistics Improvement Plan (2026-09-29)

**Status:** IMPLEMENTATION-READY (wuffagent). The plan body below is code-level
(file / struct / call-site anchors verified against the tree on 2026-09-29;
line numbers are current at writing time — re-grep the symbol per item).
Companion to `self-improvement-gaps-3.md` (the improvement *loop* over these
metrics — this plan is the metrics/statistics side: what is recorded, what can
be computed, and what the user/agents can see).

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

## Implementation plan

### Conventions (apply to every item)
- **Serde tolerance:** every new field is `#[serde(default)]`; every reader
  treats a missing value as *unknown*, never as zero (a 50% unknown-model
  share is a real signal, not noise to drop).
- **Commits:** one commit per item, message `stats(<id>): <what>`, made
  immediately after `cargo test --workspace` is green with no new warnings
  (M: drive — never leave artifacts untracked).
- **Test seams (existing, reuse — never the real `~/.wuffagent`):**
  - metrics: `MetricsLog::new(tempdir)` / `set_metrics_dir_for_testing`
    (process-global lock, like `CONFIG_PATH_LOCK`);
  - usage: `ChatClient::set_usage_recorder` + a mock SSE server
    (pattern: the tests in `run_eval.rs` — `spawn_mock_sse`/`mock_client`).
- **Restart:** core-crate changes require a WuffAgent restart — done
  **after each phase** (dual-target self-restart: omit `build_cmd`/
  `exe_path`), then verify the live build path (the running exe may be the
  other, stale target build).
- **Mechanical fallout — handle in the SAME commit that triggers it:**
  - `RunStats` (agents/types.rs:85) is currently `Copy`. Items 1a/1d add a
    `Vec` → drop `Copy` (keep `Debug, Clone, Default, PartialEq`); grep
    `RunStats` for implicit-copy uses (improver code, `agent.run_stats()`
    call sites in run_eval.rs / mod.rs — a clone is fine there).
  - Exhaustive `MetricsLine::Run { … }` constructions that must gain the new
    fields when the variant changes: `log_run` (metrics.rs:555),
    agents/metrics/tests.rs:40/131/178/244,
    tools/builtin/improvement/metrics.rs:492 (+ its `record_run` helper at
    :442). Re-grep `MetricsLine::Run {` per item — line numbers drift.

### Phase 1 — writers (5 commits, in this order: 1a → 1e → 1d → 1b → 1c)
1d depends on 1a's `tools` vec (for `tools_ms`); the rest are independent.

**1a. Per-tool histogram on run lines (A1)** — `stats(1a)` — **DONE**
Files: `agents/types.rs`, `agents/agent/tool_calls.rs`, `agents/metrics.rs`.
1. types.rs — new type + `RunStats` extension:
```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolStat {
    pub name: String,      // tool name; "__other__" is the fold bucket
    pub calls: u32,
    pub errors: u32,       // "Error: ..." outputs
    pub duration_ms: u64,
}
// RunStats: drop `Copy`; add `pub tools: Vec<ToolStat>`
impl RunStats {
    /// Aggregate one tool call. O(n) scan over ≤32 entries; allocates only
    /// for first-seen names. >32 distinct names fold into "__other__".
    pub fn bump_tool(&mut self, name: &str, error: bool, ms: u64) { /* … */ }
}
```
2. tool_calls.rs — at the existing per-call sites where
   `stats.tool_errors` is incremented (inside BOTH `run_native_tool_calls`
   and `run_text_embedded_calls`): wrap the call in `let t = Instant::now();`
   and after it add
   `stats.bump_tool(tool_name, output.starts_with("Error:"), t.elapsed().as_millis() as u64);`
3. metrics.rs — `MetricsLine::Run` gains `#[serde(default)] pub tools: Vec<ToolStat>`.
   `log_run` (metrics.rs:542) changes its loose `tool_calls`/`tool_errors`/
   `verification_attempts` params to `stats: &RunStats` (cleaner for 1d too);
   the free writer hook `record_run` (metrics.rs:912) mirrors it. Update the
   exhaustive-construction sites listed in Conventions.
Tests: `bump_tool` aggregation (calls/errors/summed ms), `__other__` fold at
33 distinct names; serde roundtrip with `tools`; a legacy line (no `tools`)
deserializes to `tools.is_empty()`; existing metrics tests green after the
`log_run` signature change.

**1e. run_id + session_id join keys (A2)** — `stats(1e)` — **DONE**
Files: `agents/metrics.rs`, `agents/agent/loop.rs`, `client/mod.rs`,
`usage/recorder.rs`.
1. metrics.rs — `Run` gains `#[serde(default)] pub run_id: String` and
   `#[serde(default)] pub session_id: String`; `Trim` gains
   `#[serde(default)] pub run_id: String`. The free `record_trim` hook gains
   a `run_id: &str` param — its 3 call sites (loop.rs:299/460/634) are all
   inside `run_llm_loop`, where the id is in scope.
2. loop.rs — top of `run_llm_loop`:
   `let run_id = format!("{}-{}", chrono::Utc::now().timestamp_millis(), self.config.name);`
   (readable; no uuid call needed — the dep exists anyway). Thread into
   `record_run(…, run_id, session_id)` (`session_id` from
   `self.session_id().unwrap_or_default()`) and into `record_trim`.
3. client/mod.rs + usage/recorder.rs — `UsageEntry` gains
   `#[serde(default)] pub run_id: String`. `ChatClient` gains
   `run_id: Arc<Mutex<Option<String>>>` + `pub fn set_run_id(&self, id: Option<&str>)`
   — same stamping pattern as `set_agent_name` (client/mod.rs:287); the loop
   stamps it at run start and clears (`None`) at run end. `record_usage`
   (client/mod.rs:318) stamps
   `run_id: self.run_id.lock().unwrap().clone().unwrap_or_default()`.
Tests: mock SSE + injected recorder → the written `UsageEntry` carries the
stamped `run_id`/`session_id`; metrics roundtrip with/without ids; legacy
`Trim` line → `run_id == ""`.

**1d. Duration split (A6)** — `stats(1d)` — **DONE**
Files: `agents/types.rs`, `agents/agent/loop.rs`, `agents/agent/verify.rs`,
`agents/metrics.rs`.
1. types.rs — `RunStats` gains `pub llm_ms: u64` (main rounds only).
2. loop.rs — wrap each round's LLM streaming await in `run_llm_loop` with an
   `Instant`; after the round: `stats.llm_ms += t.elapsed().as_millis() as u64;`
3. verify.rs — the judge call already measures `judge_started`
   (verify.rs:184). `VerificationState` gains `pub judge_ms: u64`,
   accumulated at verify.rs:194-198 from that same `Instant`.
4. metrics.rs — `Run` gains `#[serde(default)] pub llm_ms: u64` and
   `#[serde(default)] pub tools_ms: u64`. At the write site (the
   `record_run` hook): `llm_ms = stats.llm_ms + verification_state.judge_ms`
   (both in scope there) and
   `tools_ms = stats.tools.iter().map(|t| t.duration_ms).sum()`.
   `duration_ms` stays the wall-clock total (backward compat).
Tests: crafted `RunStats` + `judge_ms` → `log_run` writes the right split;
legacy line deserializes to `0/0`; assert `llm_ms + tools_ms ≤ duration_ms`
on a fixture.

**1b. model + cost_usd on run/eval lines (A3, A5)** — `stats(1b)` — **DONE**
Files: `config/mod.rs`, new `usage/cost.rs`, `client/mod.rs`,
`agents/metrics.rs`, `agents/agent` (builder + run-completion hook),
`tools/builtin/improvement/run_eval.rs`.
1. config/mod.rs:
```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub model: String,
    pub per_1M_in_usd: f64,
    pub per_1M_out_usd: f64,
}
// Config gains:
#[serde(default)] pub model_prices: Vec<ModelPrice>,  // empty = unpriced
```
   (user-editable in config.json; ships empty — no guessed prices).
2. New `usage/cost.rs`:
   `pub fn cost_usd(prices: &[ModelPrice], model: &str, prompt_tokens: u64, completion_tokens: u64) -> f64`
   — case-insensitive exact model match; `0.0` when the model is unknown or
   the table is empty (0.0 = "recorded but unpriced").
3. client/mod.rs — `ChatClient` gains `last_model: Arc<Mutex<Option<String>>>`;
   `record_usage` (where `model` is already a param, client/mod.rs:335)
   stamps it; `pub fn last_model(&self) -> String` ("" when unknown).
   Safe today: clients are per-session and agents run sequentially per
   session (same assumption as `set_agent_name`); evals use `fresh()`
   (isolated). Note the assumption in a comment.
4. metrics.rs — `Run` gains `#[serde(default)] pub model: String` and
   `#[serde(default)] pub cost_usd: f64`; `record_run` (metrics.rs:912)
   gains `model: &str, prices: &[ModelPrice]`; the agent's run-completion
   hook passes `&self.client.last_model()` and `&self.model_prices`.
   `AgentBuilder` gains `.model_prices(Vec<ModelPrice>)` → `Agent` field
   (the app's pipeline build site passes `config.model_prices.clone()` —
   grep the `AgentBuilder::new` call sites outside tests). `Eval` gains
   `model`/`cost_usd` too; `log_eval` (metrics.rs:647) gains
   `model: &str, cost_usd: f64` params.
   - DEVIATION (applied): model+cost ride in `RunStats` (new
     `model: String` / `cost_usd: f64` fields; RunStats drops `Eq`) instead
     of `record_run` params — `log_run`/`record_run` keep their signatures,
     so the ~15 test call sites are untouched. The loop stamps them at run
     end from `client.last_model()` + the `Agent`'s price table (the
     AgentBuilder `.model_prices(...)` path from the plan is unchanged).
5. run_eval.rs — `RunEvalTool` gains a `model_prices: Vec<ModelPrice>`
   field (extend its single `RunEvalTool::new` construction site in the app);
   `EvalOnce` gains `model: String` (from `client.last_model()` next to the
   existing `agent.run_tokens()` at run_eval.rs:122); the three `log_eval`
   call sites (:323/:355/:367) pass `model` +
   `cost_usd(&self.model_prices, &r.model, r.tokens_in, r.tokens_out)`
   (error/timeout paths: `""` / `0.0`).
Tests: `cost_usd` table (match, case-insensitivity, unknown model, empty
table, arithmetic); `log_run` writes model+cost (tempdir); the existing
`run_eval` mock test extended: the Eval line carries a model + `0.0` cost
(no prices configured).

**1c. Eval score via judge-prompt extension (A4)** — `stats(1c)` — **DONE**
Files: `agents/agent/verify.rs`, `agents/metrics.rs`,
`tools/builtin/improvement/run_eval.rs`.
1. verify.rs — extend `VERIFICATION_SYSTEM_PROMPT` (verify.rs:75) with one
   final sentence: *on the LAST line, respond with `SCORE: <x>` where x is a
   decimal 0.0–1.0 for how well the response satisfies the request.*
   (The NEEDS_FIX/VERIFIED detection is substring-based; a score line cannot
   flip it — the score contains no verdict tokens.) New pure fn
   `parse_judge_score(text: &str) -> Option<f64>`: case-insensitive find of
   `SCORE:`, parse the following float, clamp to `0..=1`, `None` when
   absent/malformed. `VerificationVerdict` gains `pub score: Option<f64>`;
   `verify_tool_outputs` sets it from the parsed response on BOTH outcome
   branches; the no-tool-outputs shortcut returns `Some(1.0)` (derived).
2. metrics.rs — `log_eval` (metrics.rs:647) gains `score: Option<f64>`
   (the `score` field already exists on `MetricsLine::Eval` — writer only).
3. run_eval.rs — `EvalOnce` gains `score: Option<f64>` (from
   `verdict.score`); all three `log_eval` call sites pass it
   (error/timeout → `None`); the test `MockLlm` response gains a
   `SCORE: 0.9` line and the table test asserts the Eval line stored
   `Some(0.9)`.
Tests: `parse_judge_score` table (valid, `>1` clamp, negative, missing,
garbage after colon, lowercase); eval line stores the score end-to-end.

### Phase 2 — analysis + cheap reads (4 commits)

**2a. Incremental metrics reader + egui cache (D1)** — `stats(2a)` — **DONE**
- New `MetricsLogReader` in `agents/metrics.rs` (or `agents/metrics/reader.rs`):
  port the `UsageLogReader` pattern (grep it in `usage/`): byte-offset poll,
  torn-line buffer, shrink/rotate → full rescan.
- egui: the agent editor (agent_config.rs:521 block) does
  `summary_since(name, None)` + `recent(5)` = two full scans **per frame**.
  Add to the egui app struct:
  `metrics_cache: Mutex<HashMap<String /*agent*/, (u64 /*len*/, SystemTime, Vec<MetricsLine>)>>`
  with a helper that re-reads only when (len, mtime) changed. Add
  `MetricsSummary::from_lines(&[MetricsLine])` so the cache path and the
  file path share one computation; the editor builds its block from the
  cached vec.
- `ReadMetricsTool` keeps `read_all` (one-shot, agent-facing).
Tests: append → poll returns exactly the new line; half-written line buffers
and completes next poll; file truncated → rescan; `from_lines` matches
`summary_since` on the same fixture.

**2b. Shared bucketing for the metrics store (B2, part of D3)** — `stats(2b)` — **DONE**
- New module `wuffagent-core/src/stats/bucket.rs`: move the `Granularity`
  enum + zero-filled window math out of `usage/stats.rs` (usage re-exports
  so its call sites/tests stay green).
- metrics.rs:
  `pub fn bucket_summary(&self, agent: &str, granularity: Granularity, since: Option<DateTime<Utc>>) -> Vec<BucketSummary>`
  with `BucketSummary { start, runs, tool_errors, gave_up, verified_after_retry, tokens_in, tokens_out, duration_ms_sum }`
  — one `read_all` + the shared bucket fn.
Tests: zero-filled windows on a sparse fixture (mirror the usage bucket
tests), DST-safe boundary case ported from `usage/stats.rs`.

**2c. Percentiles + comparison window (B1, B3)** — `stats(2c)` — **DONE**
- Pure `fn percentile(sorted: &[f64], p: f64) -> Option<f64>` (linear
  interpolation; unit-tested incl. empty/1-element).
- New `MetricsReport` (metrics.rs) = everything `MetricsSummary` has, plus:
  `p50_duration_ms, p95_duration_ms, p50_tokens, p95_tokens`
  (`tools_ms`-free: per-run `tokens_in+tokens_out`),
  `tool_stats: Vec<ToolStat-like {name, calls, errors, avg_ms}>` (from
  `Run.tools`), `model_mix: Vec<(String model, u32 runs)>`.
  `pub fn report(&self, agent: &str, since: Option<…>, end: Option<…>) -> MetricsReport`.
- `pub fn compare(&self, agent: &str, days: u32) -> (MetricsReport /*current*/, MetricsReport /*previous*/)`
  — current `[now-days, now)` vs the same-length preceding window, both via
  the same windowed reader (`summary_between`/`report`) so the numbers
  match what the I5 effect check computes.
Tests: percentile edges; `compare` on the 4-day fixture (metrics/tests.rs:178
shape) → correct window split at the boundary.

**2d. `read_metrics` v2 (E1, E2)** — `stats(2d)`
- `tools/builtin/improvement/metrics.rs` (`ReadMetricsTool`): new optional
  param `compare: bool` (default false). Per-agent output gains:
  - `Tools (top by errors):` — top-5 from `report.tool_stats`
    (`name, calls, err%, avg ms`);
  - `Percentiles: p50/p95 duration, p50/p95 tokens`;
  - when `compare=true`: `Window: Nd vs previous Nd` — runs, error rate
    (Δ in pp), gave_up, tokens, cost (when priced).
- `list_improvement_status` (grep the tool file): when the evidence gate
  fired via the metric-delta path, render the actual deltas — recomputed
  with the SAME `MetricsLog::compare` primitive the gate uses (share the
  fn, so the two can't diverge).
Dogfood gate: call `read_metrics(agent=…, compare=true)` live and confirm
the new sections render from the real store.

### Phase 3 — user-facing surface (4 commits)

**3a. Fleet dashboard panel (C1)** — `stats(3a)`
- New `wuffagent-egui/src/ui/dashboard.rs`: window toggle (7d/30d, default
  30d); per-agent KPI card row (runs, outcome %, error rate, tokens + $,
  last activity — all from `report`/`compare`); 30-day trend chart (runs
  bars + error-rate line; reuse the usage panel's chart drawing — extract a
  shared draw fn into `ui/charts.rs` if the coupling is awkward); a compact
  loop-status table. The core fn behind `read_metrics status=true` is
  currently tool-local in improvement/metrics.rs — extract it to core so
  UI and tool share one implementation.
- Register the panel next to the usage panel mount (grep `usage_panel` in
  the egui layout code).
**3b. Agent editor windowing (C2)** — `stats(3b)`
- agent_config.rs:521 block: 7d/30d/all-time toggle; per-tool table
  (`report.tool_stats`); outcome mini-chart; all backed by 2a's cached vec
  (no full scan per frame).
**3c. Usage panel filters + cost (C3)** — `stats(3c)`
- usage_panel.rs: agent + model dropdown filters (filter the loaded
  in-memory lines — both fields exist per line; distinct values feed the
  dropdowns); a cost card summing `cost_usd(config.model_prices, line.model,
  prompt, completion)` over the window, with an "unpriced tokens: N" note
  for lines whose model is missing from the table.
**3d. Per-run user feedback (A7 — gaps-3's 3c, done here: UI already open)**
— `stats(3d)`
- metrics.rs: `Feedback` gains `#[serde(default)] pub run_id: Option<String>`
  (legacy lines → `None` = message-level); new `log_feedback_run(agent,
  run_id, up)`. Agent editor (3b's run table): 👍/👎 buttons per recent run
  row → writes the line. `MetricsReport` counts run-level vs message-level
  separately.

### Phase 4 — joins, lifecycle, export (4 commits)

**4a. Cross-store join views (B4, B5)** — `stats(4a)`
- Core fn `run_detail(agent: &str, run_id: &str) -> Option<RunDetail>` with
  `RunDetail { run, rounds: Vec<UsageEntry>, feedback: Vec<FeedbackLine> }`:
  `usage.jsonl` filtered by `run_id` (one read; on-demand only) + the run's
  own tool stats + any run-level feedback.
- Surface: agent editor (a run row expands to its rounds: model, tokens,
  thinking_chars per round) and a `read_metrics` param `run_id: Option<String>`.
- Correlations into `MetricsReport` (cheap, from existing fields):
  gave_up share of runs with ≥1 trim vs without; retry-rate (per-bucket,
  from `bucket_summary`).
**4b. Rollup + rotation (D2)** — `stats(4b)`
- Config: `#[serde(default = "default_retention")] pub metrics_retention_days: u32`
  (default 90).
- App startup (egui main, once per calendar day, marker file
  `metrics/.rollup-state`): for each agent file, for each fully-elapsed day
  older than retention that has no rollup yet: write
  `metrics/rollups/<agent>-YYYY-MM-DD.json` (day summary in
  `MetricsReport` shape incl. tool + model maps), then prune raw `run` lines
  older than retention (feedback/skill_use/trim/check/eval lines are kept —
  small, high-signal).
- `report`/`summary_between` consult the rollup files for days before the
  raw file's min ts, so pruning never loses an aggregate the dashboards or
  the loop read.
Tests: temp store with synthetic old lines (test-override dir — never the
real `~/.wuffagent`, per the metrics-pollution lesson): day rolls up once,
raw run lines prune, out-of-window aggregates still answer.
**4c. Export (D4)** — `stats(4c)`
- `ReadMetricsTool` gains `export: Option<String>` (`"csv"|"json"`): windowed
  raw lines (all kinds, `agent` column added for fleet) →
  `~/.wuffagent/exports/metrics-<agent|fleet>-<N>d-<ts>.<ext>`; the output
  reports the file path. JSON = array of raw lines; CSV = flattened with a
  `kind` column + ISO8601 `ts`.
**4d. Schema version (D3)** — `stats(4d)`
- `#[serde(default)] pub v: u32` on all 7 `MetricsLine` variants and on
  `UsageEntry`; writers set `v: 1`. Purely the migration anchor — tolerant
  parsing behavior unchanged.

### Resolved decisions (no open questions)
- Verification time folds into `llm_ms` (as `judge_ms`); no third field.
- `tools_ms` is a STORED field (= sum of the `tools` vec) so summaries never
  re-parse the vec.
- Price table ships EMPTY (cost 0 = unpriced); wrong hardcoded prices
  mislead more than no price.
- `run_id` format: `<timestamp_millis>-<agent>` (readable in the log).
- Eval score: extend the existing judge prompt (one LLM call, not a second);
  the no-tool shortcut derives `Some(1.0)`.
- Phase-1 order: 1a → 1e → 1d → 1b → 1c (only 1d needs 1a).

## Order & gates
Phase 1 → 2 → 3 → 4 (2 needs 1's fields; 3 needs 2's primitives; 4 needs 1e).
Per item: `cargo test --workspace` green, no new warnings, one commit.
After each phase: restart WuffAgent (core changed — dual-target self-restart,
omit `build_cmd`/`exe_path`) and verify the live build path; commit any
follow-ups immediately (M: drive drops untracked files).

## Verification targets (definition of done for the round)
- `read_metrics` answers "which tool fails most for agent X, and did it get
  worse this week?" — per-tool table + comparison window, from real data.
- The fleet dashboard renders per-agent KPIs + 30-day trends; the agent
  editor shows windowed stats without a full-file scan per frame (verifiable
  by code inspection of the cached reader).
- Token spend shows a USD figure wherever it shows tokens (priced models),
  and eval lines carry real scores.
- A `gave_up` run can be traced to its individual LLM rounds via `run_id`.
- 90+ day old raw run lines are rolled up + pruned without losing any
  aggregate the dashboards or the loop read.

## Notes / hazards
- **Metrics pollution:** all writer changes get tests under the temp-dir /
  test-override seams listed in Conventions. The run-writer is in the hot
  path; the per-tool histogram must stay a cheap increment (linear scan over
  ≤32 entries, no per-call allocation except first-seen names).
- **Back-compat:** every Phase-1 field is `#[serde(default)]` and every
  reader tolerates its absence (the 4c/1c/2b pattern already in metrics.rs).
  Old lines get `model=""`, `cost_usd=0.0`, `run_id=""`, empty tool lists —
  summaries treat those as "unknown", not zero.
- **`RunStats` loses `Copy`:** grep every use before/after 1a; clone is the
  expected replacement (the struct stays small).
- **Judge prompt side effect (1c):** the LIVE loop's judge will also start
  emitting a `SCORE:` line in its reason text — harmless (reasons are
  truncated in logs/lessons), but verify NEEDS_FIX detection in a test.
- **`last_model` stamping (1b):** only safe because clients are per-session
  and runs are sequential per session (same invariant as `set_agent_name`);
  document it on the field.
- **M: drive:** mtimes lie, untracked files vanish — commit each item's
  artifacts; verify file existence with shell, not `list_dir` freshness.
