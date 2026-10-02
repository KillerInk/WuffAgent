# Perf pass: llama.cpp server integration (new code since the November perf plan)

**Date:** 2026-07-01
**Scope:** the ~3,600 lines added by the llama.cpp server integration work
(`server/*`, `client/sse.rs`, `types/usage.rs`, `types/activity.rs`,
`trimming/brief.rs` consumers) — legacy code was already covered by
`autoplans/performance-optimization.md`.

## Findings (audit)

Clean (no action needed):
- `types/usage.rs`, `types/activity.rs` — cheap structs, `LlmActivityTracker`
  emits at ~5 Hz max (throttled, not per token).
- `trimming/brief.rs` — trim path, not per-frame; `enforce_total_cap` is O(n²)
  only over a ≤24-item list.
- UI status bar reads `server_status` behind `lock().ok()` every frame —
  small snapshot, cheap.
- `progress.rs` `parse_progress` — O(1), fine.

Issues, by impact:

1. **P1 (high)** — `server/status.rs`: a **new `reqwest::Client` is built on
   every 3 s poll** by `poll_server_status`, plus one more per
   `fetch_n_ctx_train` call; `ServerManager::wait_for_ready` builds one per
   500 ms probe (model loads take minutes → dozens of client constructions).
   Client construction sets up the connection pool / TLS context / background
   threads; it should be built once. Reuse also enables HTTP keep-alive.
   **Fix:** build the client once per monitor task / per wait loop, pass
   `&Client` down.
2. **P2 (stability)** — `server/mod.rs`: the spawned llama-server's
   stdout AND stderr pipes are piped but **never drained** (`monitor_output`
   is the only reader, is dead code, and would only read stdout).
   llama-server logs to stderr; once the OS pipe buffer fills, the server
   blocks on `write()` and generation stalls mid-session. **Fix:** at spawn,
   take both pipe handles and run drain tasks that forward lines to
   `tracing::debug!` (debug, to avoid log flooding — see log-flood memory).
   Remove the dead `monitor_output`.
3. **P3 (per-token)** — `client/sse.rs` `process_sse_line`: performs the
   `choices[0].delta` 3-hop `Value` hash-lookup chain **5× per streamed
   line**. **Fix:** bind `delta` once.
4. **P4 (per-token)** — `client/sse.rs` `stream()`: `extract_model`
   re-parses the full line JSON every iteration until a `model` field
   appears; for OpenAI-compat backends that never send one, that's a second
   full parse of every streamed line. **Fix:** cheap `contains("\"model\"")`
   gate first.
5. **P5 (trivial)** — `server/status.rs`: the `/props` body is parsed into a
   `Value` **twice** (once for `n_ctx`, once for `model`). **Fix:** parse
   once, reuse.

Deferred (measured as negligible):
- `poll_server_status` runs its 4 GETs sequentially (~2 ms saved every 3 s
  if parallelized via `futures::join!`). Skip unless the poll interval drops.
- `String::from_utf8_lossy(&bytes)` in the SSE reader: keep, but only if it
  doesn't clone (verify at build; switch to `str::from_utf8_lossy` if it
  allocates on valid UTF-8).
- Duplicated `test_parse_progress` in `server/tests.rs` and
  `server/progress/tests.rs` — cosmetic.

## Implementation (this session) — DONE (commit 88a649f)

- [x] Plan file committed
- [x] status.rs: client reuse (P1) + single /props parse (P5)
- [x] mod.rs: drain pipes at spawn (P2) + wait_for_ready client (P1) + remove monitor_output
- [x] sse.rs: single delta bind (P3) + model gate (P4)
- [x] `cargo test -p wuffagent-core` (887+2+1 ok, exit 0) + `cargo check --workspace --all-targets` (exit 0)
- [x] Commit 88a649f

## Verification

Full wuffagent-core suite green (invariant: whole suite, not a count);
`cargo build` of wuffagent-egui compiles (egui consumes the changed
signatures indirectly only — no API surface change except the removed
`ServerManager::monitor_output`, which has zero callers).
