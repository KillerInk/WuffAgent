# Test Coverage Plan (wuffagent-core + egui)

Created: 2026-07-09. Status: **DONE** (2026-07-09), one item deferred (7).

Goal: close the biggest untested gaps in the workspace with small, modular,
self-contained test additions (no production refactors except where a test
exposed a real bug).

## Results

1. [x] **SSE parser hardening + tests** (`client/sse.rs`)
   - `client/tests/ready.rs`: 3 new tests for `ToolCallTracker` /
     `fire_tool_ready_if_complete` — multi-call interleaving (A→B→A with
     index-only continuation chunks), index-only continuation resolution,
     last-call-never-fires.
   - `looks_like_complete_json` edge cases (escaped quotes, nested arrays,
     braces inside strings, unterminated strings).
   - **BUG FOUND & FIXED**: the depth counter counted braces/brackets INSIDE
     strings, so `{"s": "x}"}` was reported incomplete (missing early
     tool-ready firing; inline fallback still made it correct). Fix:
     `'{', '['` / `'}', ']'` arms now guarded by `!in_string`.
2. [x] **MCP manager tests** (`tools/mcp/manager/`)
   - `allowlist_filters_tools_on_connect_and_refresh` (python mock server in
     `tools/mcp/mod.rs` integration_tests): `allowed_tools` restricts
     registration on `connect_async` AND `refresh_tools_async`; snapshot
     reflects the filtered list; `remove_server_sync` unregisters.
   - **BUG FOUND & FIXED (pre-existing)**: flaky race in
     `tools/builtin/mcp/tests.rs` — two file-based tests share the
     PROCESS-GLOBAL config-path override and ran in parallel, each reading
     the other's temp file (`["one"]` vs `["a","b"]`). Serialized with a
     static `CONFIG_PATH_LOCK`.
3. [x] **Memory manager dedup** — already done pre-session: `add_batch`
   reuses `is_near_duplicate` (manager.rs:263); unit tests for
   `content_overlap`/`is_near_duplicate`/`clean_stale_entries` in
   `memory/manager/tests.rs`.
4. [x] **Session model tests** — already done: `sessions/model/tests.rs`
   (normalize/sanitize invariants, round-trip, corrupt-file error path).
5. [x] **`#[allow(dead_code)]` cleanup** in `agents/engine.rs` — already
   done (no marker left; one pre-existing in `agents/agent/mod.rs:31` is a
   `pub` item, kept).
6. [x] **Trimming module tests** — already done: `trimming/filestate/tests.rs`,
   `trimming/summarizer/freshness/tests.rs`.
7. [ ] **HTTP client streaming tests** (`client/http.rs`) — **DEFERRED**:
   needs a mock SSE server (mockito or a local tokio hyper server emitting
   canned SSE lines; verify conversation accumulation, usage extraction,
   `[DONE]`, mid-stream cancellation; non-streaming 4xx/5xx error mapping).
   Self-contained follow-up; `process_sse_line` itself is now well covered.
8. [x] **UI settings tests** — already done: `ui/settings/tests.rs`
   (24 tests).
9. [x] **Empty assistant placeholder handling** — already done:
   `Agent::is_storable` (memory_sync.rs), `Session::normalize`
   (sessions/model.rs:148-168), `client/http.rs:127` request-building skip,
   regression test in sessions/model/tests.rs.
10. [x] **Final gate**: `cargo test --workspace` green
    (wuffagent-core 560 + wuffagent-egui 24 + 1 doctest, EXIT=0).

## Notes for the deferred item (7)

- `process_sse_line` is a pure function — the remaining gap is the
  `stream_message` line-buffering/cancellation loop in `http.rs`.
- Cheapest approach: a small `tokio` TCP listener test server (no new
  dependency) that writes canned `data: ...` lines, or add `mockito` as a
  dev-dependency if its streaming support is sufficient.
