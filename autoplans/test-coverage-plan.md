# Test Coverage Plan (wuffagent-core + egui)

Created: 2026-07-09. Status: **DONE** (2026-07-09, all 10 items).

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
7. [x] **HTTP client streaming tests** (`client/http.rs`) — DONE (second pass):
   `client/tests/http_stream.rs` (5 tests) with a hand-rolled one-shot
   HTTP/1.1 server on `tokio::net::TcpListener` (no new dependency):
   `send_message` success (content/usage/timings/model/tool_calls/
   thinking_chars), 429 → `Error::Http` with body, invalid JSON →
   `Error::Json`; `stream_message` full SSE (body flushed in 3 pieces to
   exercise line-buffer reassembly; content+thinking+2 tool calls incl.
   index-only continuation; early ready-firing of tc1; usage+model from
   final chunk; `[DONE]`); cancellation mid-silent-stream →
   `Error::Cancelled` with partial content kept.
8. [x] **UI settings tests** — already done: `ui/settings/tests.rs`
   (24 tests).
9. [x] **Empty assistant placeholder handling** — already done:
   `Agent::is_storable` (memory_sync.rs), `Session::normalize`
   (sessions/model.rs:148-168), `client/http.rs:127` request-building skip,
   regression test in sessions/model/tests.rs.
10. [x] **Final gate**: `cargo test --workspace` green
    (wuffagent-core 565 + wuffagent-egui 24 + 1 doctest, EXIT=0).

## Test notes

- The hand-rolled SSE test server (http_stream.rs) writes canned HTTP/1.1
  responses on a local `TcpListener`; the streaming body is flushed in
  separate writes/pieces so the client line buffer must reassemble lines
  split across TCP writes.
- Wire detail the tests encode: streamed tool-call fragments carry
  `type:"function"` + `id` only on the FIRST fragment; continuations are
  index-only. Canned SSE lines must be balanced JSON — one missing `}`
  makes the whole line "unparseable" and the tool call silently vanishes.
