# Modularization Plan — WuffAgent

Goal: reduce file/struct size and duplication so each module has one clear responsibility.
Method: process the largest / most entangled files step by step; this plan is updated as findings land.
Baseline: working tree clean at `db309fe` (2026-07-21).

## Findings (analysis pass — 2026-07-21)

### F1. `agents/metrics.rs` (82 KB, 2124 lines) — god file, 5 responsibilities
Structure (line numbers):
- L51–386  schema: `RunOutcome`, `FeedbackKind`, `MetricsLine` (+ `describe`, `ts`, `percentile`)
- L412–441 `RollupTotals`; L443–580 `build_report`/`metrics_report_from_lines`
- L582–655 `RunDetail`; L656–681 `windowed_summary`, `bucket_summary_from_lines`
- L682–940 report structs: `ReportToolStat`, `MetricsReport`, `MetricsRollup`, `TrimCorrelation`, `BucketSummary`, `MetricsSummary` (+`AddAssign`)
- L943–988 `agent_file_name`, `line_ts`
- L1041–1838 `MetricsLog` (~40 methods, 4 sub-responsibilities:
  append/`log_*`, read (`read_all`/`recent`/`agent_names`/`lines_since`),
  aggregate (`summary_*`/`report`/`run_detail`/`loop_cost_since`), rollup (`roll_up_and_prune`/`maybe_daily_rollup`/`rewrite_agent_file`))
- L1839–1916 free `record_*` fns; L1917 `mod reader` (already split, 146 lines, fine)
- L1924–2024 `accumulate` + second `impl MetricsLog`
- L2025–2123 fleet loop status structs + `fleet_loop_status`
- `metrics/tests.rs` 78 KB (test file is as big as the code)

→ Split into a directory module `agents/metrics/`:
  `mod.rs` (re-exports), `schema.rs` (lines+enums), `summary.rs` (MetricsSummary/RollupTotals/accumulate/windowed),
  `report.rs` (report structs + build_report + RunDetail), `log.rs` (MetricsLog), `rollup.rs` (rollup/prune),
  `fleet.rs` (fleet loop status). `reader.rs` + `tests.rs` move along. Split `tests.rs` per sub-module too.
  NOTE: `MetricsLog::report`/`summary_*` stay in `log.rs` as thin delegations to `summary.rs`/`report.rs` fns so
  public API is unchanged (all callers use `MetricsLog` or the free fns).

### F2. `line_ts` duplicated
- `agents/metrics.rs:971` (returns `&DateTime<Utc>`) and `tools/builtin/improvement/metrics.rs:428`
  (returns owned copy, identical match). → make one `pub` helper on `MetricsLine` (`fn ts(&self)` already
  exists at metrics.rs:372 returning owned!) and delete both free fns.

### F3. `tools/builtin/improvement/metrics.rs` (62 KB, 1433 lines) — one tool, 3 concerns
`ReadMetricsTool` impls at L69–425 (agent/fleet report rendering), L439–488 (`run_detail_report`),
L489–645 (`Tool` impl), L646–722 (compare), L723–782 (CSV export `export_csv`/`csv_cell`), tests L784+.
→ Split: `metrics/tool.rs` (Tool impl + params), `metrics/report.rs` (render fns, take `&MetricsLog`),
`metrics/export.rs` (csv/json). Keep as sub-module dir `improvement/metrics/`.

### F4. `agents/improvement.rs` (55 KB, ~1230 lines) — mixed bag of free fns
- L37–196 backoff/effect verdict + evidence collectors (cohesive: loop policy)
- L302–368 window/eval/trajectory/lesson formatting (evidence formatting)
- L394–746 `suggest_improvements` — ONE ~350-line async fn (prompt building + call + parse)
- L747–1000 fleet suggestion helpers (sanitize, dice, fleet_summary_line, fleet_evidence_json)
- L995 `truncate_to` + L1224 `truncate_for_evidence` — two truncate variants in the same file
→ Split: `improvement/policy.rs` (backoff, effect verdict), `improvement/evidence.rs` (collectors + formatting),
`improvement/suggest.rs` (suggest_improvements broken into `build_suggestion_prompt` + call + parse),
`improvement/fleet.rs` (fleet helpers). Also `suggest_fleet_improvements` lives in `memory/manager.rs` (F6).

### F5. `memory/manager.rs` (42 KB) — MemoryManager has TWO jobs
Job A (core memory, keep): search/get_*/add/add_batch/update/delete/find/supersede/count/cleanup/save
(L238–311, L609–765, L819+).
Job B (improvement-loop state, extract — 15 pub methods + docs L112–607 + L905–999):
suggest_improvements, run_improvement_check, suggest_fleet_improvements, run_fleet_improvement_check,
has_new_improvement_evidence, record_improvement_check, agent_improvement_state, record_agent_task_completed,
agent_improvement_due, record_agent_improvement_check, record_effect_verdict,
has_new_agent_improvement_evidence, agent_metric_evidence, improvement_status,
+ `ImprovementStateDoc`, `AgentStateRec`, `metric_evidence_reason`, `block_on_improvement`.
→ New module `memory/improvement_state.rs` with an `ImprovementStateStore` (owns the doc JSON inside the
memory dir); `MemoryManager` holds it and delegates (keep thin public delegations so callers don't change).
Also extract dup-detection (`content_overlap`, `is_near_duplicate`, `clean_stale_entries` L846–904)
into `memory/dedup.rs`.

### F6. `egui/ui/agent_config.rs` (59 KB, ~1400 lines) — one dialog struct, 30 methods
`AgentConfigDialog::show` + draw_* at L308–1082, metrics views L567–892, evals L893–1001,
tools-tree L22–155 + L1288–1392, save/delete L1055–1218, form state L1235–1287.
→ Split into `ui/agent_config/`: `mod.rs` (struct + show + save/delete), `tools.rs` (ToolNode tree),
`metrics_view.rs` (cached_metrics_lines, draw_tools_and_metrics, draw_agent_metrics_block, draw_run_detail),
`evals.rs` (draw_evals, start_eval_run, mark_evals_finished), `editor.rs` (basic/shell/handoff groups + form state).
Methods become free fns taking `&mut AgentConfigDialog` (Rust allows private-field access within the parent module tree).

### F7. `egui/ui/improvements/draw.rs` (53 KB) — `ImprovementsPanel::draw` is ONE 780-line fn (L66–844)
No internal section markers; inline UI drawing only. → Break `draw` into per-panel-section fns
(suggestions list, skills, memory, loop status header, filters…) each `fn draw_x(ui, panel, …)`.
Helper fns at L845–923 stay. Highest single-blob value in the UI crate.

### F8. `trimming/brief.rs` (48 KB, 1110 lines) — code is only L1–746 (746 lines), tests inline L747+
Responsible and cohesive (brief lifecycle: render/parse/note ops/polish). Low value to split the code;
→ just move inline tests to `brief/tests.rs` for consistency with the codebase pattern. Optional:
extract `parse_polish`/`polish_request`/`polish_span_text` (L375–479, LLM-polish pipeline) to `brief/polish.rs`.

### F9. `agents/agent/loop.rs` (40 KB) — `impl Agent` L66–732 (~670 lines)
This is the core LLM tool loop; sibling modules (execute, tool_calls, tool_exec, verify, prompt, inject,
toolcall_parse) already exist. The remaining blob is the actual orchestration — LEAVE AS IS (splitting the
loop itself risks churn for little clarity). Optional micro: `RunIdGuard` (L746+) + `truncate_note` (L733)
could move out.

### F10. Truncate helpers scattered (6+ variants)
`agent/mod.rs:31 truncate_chars` (pub), `improvement.rs:995 truncate_to`, `improvement.rs:1224
truncate_for_evidence`, `loop.rs:733 truncate_note`, `search.rs:73 truncate_line`,
`mcp/tool.rs:233 truncate_to_bytes`. Semantics differ (char-boundary, ellipsis style) — do NOT merge blindly.
→ Create `wuffagent-core/src/util/text.rs` with `truncate_chars` (char-boundary + `…` suffix, the common case)
and migrate the byte-safe + plain ones; leave byte-oriented `truncate_to_bytes` where it is. Low priority.

### F11. `tools/builtin/improvement/status.rs` (37 KB) — one tool, `Tool` impl L53–366 + tests
Single concern (render improvement status). Borderline; only split if it grows. No action for now.

## Phases (each = one commit, `cargo test` green before commit)
- [ ] Phase 1 — done: analysis (this file)
- [x] Phase 2 — F2: `MetricsLine::ts()` dedup — DONE: both `line_ts` free fns deleted
  (agents/metrics.rs, tools/.../improvement/metrics.rs), 6 call sites → `l.ts()`;
  `cargo test -p wuffagent-core metrics` = 70 passed; 0 failed
- [x] Phase 3 — F1: split `agents/metrics.rs` → `agents/metrics/` — DONE (67ac31b):
  `mod.rs` (3KB re-exports) + `schema.rs` (14KB) + `aggregates.rs` (24KB) + `log.rs` (37KB)
  + `fleet.rs` (4KB); `reader.rs`/`tests.rs` kept. Public API unchanged (pub use);
  816/816 core tests + 70/70 metrics tests pass, egui check clean.
  NOTE: `log.rs` is still the biggest chunk (MetricsLog 40 methods) — acceptable for now;
  split append/rollup out only if it grows.
- [x] Phase 4 — F3: split `improvement/metrics.rs` tool → `metrics/` dir — DONE (c854e77):
  `mod.rs` (40KB: struct + Tool impl + tests), `report.rs` (16KB: agent/fleet/status/run-detail
  renderers), `export.rs` (5.7KB: 4c export + CSV helpers). Report/export fns `pub(crate)`,
  `super::status::` paths → `crate::tools::builtin::improvement::status::`. 816/816 tests,
  egui clean. NOTE: mod.rs is still ~380 non-test lines (execute + schema) — fine as-is.
- [ ] Phase 5 — F4: split `agents/improvement.rs` → policy/evidence/suggest/fleet
- [ ] Phase 6 — F5: extract `ImprovementStateStore` + dedup from `MemoryManager`
- [ ] Phase 7 — F7: break `ImprovementsPanel::draw` into section fns (UI, no API change)
- [ ] Phase 8 — F6: split `agent_config.rs` dialog into sub-files (UI, no API change)
- [ ] Phase 9 — F8/F10: brief tests move-out + `util/text.rs` truncate consolidation
- [ ] Phase 10 — `cargo test` full run + update this plan with results + README/BUILD notes if module layout changed

Risks / invariants:
- Public API of `wuffagent-core` (re-exports from `agents/mod.rs`, `memory/mod.rs`) must not change; use
  `pub use` in the new `mod.rs` files.
- egui dialog splits are pure intra-crate moves — verify with `cargo test -p wuffagent-egui`.
- `metrics/tests.rs` uses `set_metrics_dir_for_testing` globals — keep them in one module (schema or log)
  and re-export; the process-global test dir must remain unique.
- Do NOT reformat whole files; keep diffs reviewable (move = git mv + `use` fixes).
