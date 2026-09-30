# Performance optimizations (2026-11-04, wuffagent)

**Status:** ✅ COMPLETE (created 2026-11-04). P1+P3 done (ad061e3), P2 done (e4dc423), P5 done (0175d36), P4 done (2d65a83), P6 done (47611e0; off-thread turn-end save + per-session save lock + narrowed conversation lock), P7 done (5e19b54; query tokenized once per search + O(1) stopword lookup). Core 827 + egui 44 tests green, `cargo check --workspace --all-targets` clean.
Audit of wuffagent-core + wuffagent-egui hot paths: the per-round agent LLM
loop, the per-chunk SSE stream, and the per-frame egui render. All findings
below are confirmed in code with file:line evidence. Ranked P1 > P2 > P3 by
cost-to-fix ratio. Items verified as ACCEPTABLE (not targets): metrics-log
per-write open (one open per run/trim, not per line), session save
frequency (per turn, not per chunk), memory search being in-memory keyword
scoring, live token-gauge updates (already O(1) delta, quadratic bug fixed
in `update_live_estimates` — see the regression test).

## Findings

### P1. Agent loop re-serializes the tool schemas every LLM round (HIGH)
`wuffagent-core/src/agents/agent/loop.rs:38-47` — `request_overhead_chars`
runs `serde_json::to_string(defs).chars().count()` on the FULL tool
definition list. It is called at `loop.rs:290` (every round, for the trim
check) and `loop.rs:464` (overflow retry). Tool definitions are invariant
for the life of one run (the allowlist is fixed at run start), yet with
~40 tools the schema is 100–300 KB of JSON re-serialized + re-char-counted
on EVERY round of the loop (a long session = 50–200 rounds).
- **Fix:** compute the schema char count ONCE before the loop in
  `run_llm_loop` (tool_defs is already built there) and pass the number
  into the trim check; keep the per-round image count (cheap scan) or fold
  it into an incremental counter. Change `request_overhead_chars` to take
  `schema_chars: usize` instead of `Option<&[ToolDefinition]>`.
- **Verify:** existing loop tests + a `criterion`-free timing check:
  `tracing::debug!` of per-round overhead time before/after, or a unit test
  asserting the schema is serialized exactly once per run (count via a test
  hook).

### P2. `messages.to_vec()` deep-clones the whole history per round (HIGH)
`wuffagent-core/src/client/chat.rs:274` — `stream_with_messages_arc` builds
`ChatRequest { messages: messages.to_vec(), tools: tools.map(to_vec), .. }`
then `serde_json::to_string(&request)`. The `to_vec()` is a deep clone of
the entire history (tool outputs, reasoning_content, base64 image payloads)
on every round — pure overhead, because the list is immediately
re-serialized into the request body and the clone is dropped. On a 10 MB
context this is a 10 MB allocation+copy per round on top of the serialize.
- **Fix:** serialize a BORROWING view: add `ChatRequestRef<'a>` (same wire
  fields, `messages: &'a [Message]`, `tools: Option<&'a [ToolDefinition]>`)
  with a hand-written or derived `Serialize` impl, and serialize that. Drop
  the `to_vec()`. (serde serializes `&[T]` fine; the owned-Vec requirement
  is only because `ChatRequest` is also used by the non-streaming path,
  which can keep the owned struct.)
- **Verify:** golden-test that `serde_json::to_string(&ChatRequestRef)` ==
  `serde_json::to_string(&ChatRequest)` for the same data (byte-identical
  wire format), plus the existing client tests.

### P3. `message_char_count` re-scanned 6–7× per round (MEDIUM)
`loop.rs:291, 299, 322, 339, 352, 395, 468` — each call is O(total chars)
with full UTF-8 decoding (`estimate_tokens` = `chars().count()`,
`trimming/summarizer/mod.rs:125-127`). Several calls happen in the same
round where the list is unchanged between them (291 vs 299: identical list;
352 vs the trim result). On a 10 MB context that is ~60 MB of char-decoding
per round, several MB of it redundant.
- **Fix:** compute `msg_chars = message_char_count(messages)` once at the
  top of the round, reuse for the trigger check + `chars_before` (=
  `total_chars - overhead`); recompute only after `trim_messages_detailed`
  / `polish_brief` mutate the list. Do NOT change `estimate_tokens` to
  `len()` yet — the calibrated chars-per-token ratio is char-based and
  switching to bytes would systematically over-trim multibyte text; track
  as a separate calibration question.
- **Verify:** loop tests; grep the round body to confirm no remaining
  redundant full scans.

### P4. egui deep-clones the streaming buffers every frame (HIGH, UI)
`wuffagent-egui/src/ui/chat_area/mod.rs:90-103` — while a session is
generating, `draw_chat_area` does
`r.chat_state.current_thinking.clone()` + `r.chat_state.stream_buffer.clone()`
on EVERY frame. egui repaints every frame during streaming (spinners,
pulsing borders/dots are time-animated), so a 500 KB thinking buffer is
cloned at the display refresh rate: O(n²) total copying per generation and
a constant multi-MB/s memory bandwidth hit. The committed-message snapshot
right above (mod.rs:35-63) already solved the same problem with staleness
gating — the streaming state just wasn't given the same treatment.
- **Fix:** extend the existing snapshot cache: store
  `(snapshot_session, snapshot_len, thinking_len, buffer_len,
  Arc<(String, String)>)` in `DisplayState`; re-clone into the Arc only
  when a length changed, otherwise reuse the Arc (per-frame cost drops to
  two length reads + an `Arc::clone`). Same pattern for the per-frame
  `active_tools.clone()` at mod.rs:107-111 (gate on
  `active_tools.len()` + a cheap generation counter or the last
  `live_output.len()`).
- **Verify:** existing UI tests + manual stream of a long thinking
  response; the WUFF_LAYOUT_DBG instrumentation already exists if needed.
 - **Done (2d65a83):** extended `DisplayState` with
   `stream_snapshot: Arc<((String, String), Vec<ActiveTool>, bool)>` +
   `stream_snapshot_key` (session, is_generating, thinking_len, buffer_len,
   revision) via `groups::stream_snapshot_key`. Because the live-output tail
   is REPLACED (not appended) and can keep the same length, `active_tools`
   is keyed on a new monotonically increasing `active_tools_revision`
   (wuffagent-core `ChatAreaState`) bumped at all 6 UI mutation sites
   (start push, progress replace, complete/error retain, complete/error
   clear); the text buffers grow-only while generating, so lengths suffice
   (zeroed when not generating). 3 new unit tests for the key; core 825 +
   egui 44 tests green.

### P5. SSE tool-ready check clones the growing tool call per delta (MEDIUM)
`wuffagent-core/src/client/sse.rs:65-88` — `fire_tool_ready_if_complete`
locks the conversation, reverse-scans to the assistant message, and
`cloned()`s the ENTIRE `ToolCall` (including the still-growing `arguments`
string) on every call; it is invoked on every text/thinking delta
(`sse.rs:212`) and every tool-call switch (`sse.rs:218`). Reasoning models
(Qwen3/DeepSeek) interleave thinking deltas with tool-arg deltas, so for a
large `write_file` call (tens of KB of args) each thinking delta re-clones
all args so far and `looks_like_complete_json` (O(args)) re-scans them:
O(n²) in argument size per large tool call, plus a mutex round-trip per
chunk.
- **Fix:** borrow, don't clone: under the lock, check
  `looks_like_complete_json(&tc.function.arguments)` on the borrowed
  string; only clone the `ToolCall` when it passes (it then fires exactly
  once per id). Optionally keep a last-checked args `len()` per id in the
  tracker and skip the balance scan entirely while the id is still the
  active one and its length is unchanged.
- **Verify:** existing sse tests; add one asserting the ready callback
  fires exactly once for a large multi-delta tool call, and (timing) that a
  100 KB args stream with interleaved thinking deltas completes in
  ~O(args), not O(args²) — a coarse `Instant` bound in a test suffices.

### P6. Turn-end session save blocks the UI thread (MEDIUM, UI)
`wuffagent-egui/src/ui/event_handler/stream.rs:113,148` →
`state.rs:227-233` — on `StreamComplete`/`StreamError` the UI thread runs
`runtime.client.save_session()`: serialize the full conversation (multi-MB
JSON, base64 images included) and write it to disk. Once per turn is
acceptable frequency, but on a big session the UI hitches exactly when the
user expects the response to finish.
- **Fix:** capture what the save needs (session id + a cloned/owned
  snapshot of the serializable state, or move the conversation vec out
  under the mutex) and `spawn` the serialize+write on the existing helper
  machinery (e.g. the `memory_runtime` or a plain `std::thread::spawn` with
  the data moved in); keep the failure `tracing::warn!` path. Care needed:
  the client's `save_queue`/debounce state (see D1 notes in
  code-design-improvements.md) — if `save_session` already coalesces via
  `save_queue`, the simplest correct fix may be to move just the file-write
  off-thread.
- **Verify:** manual: send a turn in a large session, confirm no frame
  drop at completion (WUFF_LAYOUT_DBG or the egui frame profiler); existing
  save tests.
 - **Done (47611e0):** `ChatClient::save_session_async` (client/persist.rs)
   spawns a detached `session-save` worker running the unchanged
   `sessions::persist::save_session` (thread-spawn failure falls back to a
   sync save); egui `save_session_for_async` (state.rs) is called from the
   two turn-end sites (stream.rs StreamComplete/StreamError) — edit/delete/
   sweep/stop/switch/exit `save_session_for` call sites stay synchronous.
   Two supporting core changes: (1) per-session `save_lock: Arc<Mutex<()>>`
   on `SessionState`, acquired at the top of `save_session`, serializes
   concurrent saves of the same session so an async worker and a UI-thread
   save cannot tear the shared `<id>.json.tmp` (verified
   `retry_pending_saves` drops its queue lock before the save call — no
   lock-order inversion); (2) the conversation lock in `save_session` is
   narrowed to the snapshot clone, so disk I/O + retry backoff sleeps no
   longer block streaming/agent threads. 2 new tests (async persist;
   sync+async serialization of a 2000-message session); core 827 + egui 44
   tests green. Manual frame-drop check left to the user (needs a real
   large session in the UI).

### P7. Memory search re-tokenizes the query per entry (LOW)
`wuffagent-core/src/memory/search.rs:42-45` — `keyword_score` tokenizes the
query for EVERY entry (`tokenize(query)` inside the per-entry closure at
`keyword_search`, search.rs:99-104), and `is_stopword` (search.rs:25-27) is
a linear `contains` over ~100 stopwords per token.
- **Fix:** tokenize the query once before the `entries.iter().map(...)`;
  build the stopwords as a `&[&str]` → `std::collections::HashSet` (lazy
  `OnceLock`) or a sorted slice + `binary_search`.
 - **Verify:** existing `search::tests` (score equality before/after).
 - **Done (5e19b54):** the scorer is now `keyword_score_prepared(entry,
   query_lower, query_tokens)`; `keyword_search` lowercases + tokenizes
   the query ONCE before the `entries.iter().map(...)` and scores every
   entry with the prepared values (score math unchanged — `search::tests`
   pass with their original assertions, now routed through a `score_entry`
   helper that mirrors the one-time preparation). `is_stopword` is O(1):
   the ~100-word `STOPWORDS` slice feeds a lazy
   `OnceLock<HashSet<&'static str>>` built on first use. core 827 tests
   green; `cargo check --workspace --all-targets` clean.

### P8. (Watchlist, no action)
- SSE per-line `serde_json::from_str::<Value>` (sse.rs:134): standard
  approach, fine at token rates. Revisit only if profiling shows it.
- `breaking_label` re-layout of the growing stream buffer per frame:
  inherent to live text rendering; egui's layout cache should hit when text
  is unchanged between frames. Revisit only if P4 lands and streaming is
  still janky.
- Metrics log opens the file per write (log.rs:75-77): one open per
  run/trim line, explicitly documented as intentional (no stale handles,
  trivially correct clones). Not a target.
- Session save frequency: per turn (StreamComplete/StreamError), not per
  chunk — acceptable.

## Order of work
1. **P1 + P3** (same function, ~1 focused diff in `loop.rs`) — commit.
2. **P2** (chat.rs borrowing request view + wire-format golden test) — commit.
3. **P5** (sse.rs borrow-only readiness check) — commit.
4. **P4** (egui streaming snapshot cache) — commit.
5. **P6** (off-thread turn-end save) — commit.
6. **P7** (memory search hoist) — commit.

Each step: change + tests + `git commit` before moving on. No behavior
changes intended anywhere — pure performance; the trim trigger/target math
must stay byte-identical (P1/P3 keep the same numbers, just computed less
often).
