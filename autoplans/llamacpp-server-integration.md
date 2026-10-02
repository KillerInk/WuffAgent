# Plan: llama.cpp server integration — what WuffAgent uses, what's missing, what to build

**Status:** Phase 1 DONE (status monitor, 2026-07-19); Phase 2 + 3 detailed below (next step: Phase 2)
**Date:** 2026-07-19
**Owner:** chat (general agent)
**Related:** `autoplans/statusbar-llm-activity.md` (ActivityTracker plan, already designed)

## Current state (what WuffAgent uses from llama.cpp today)

| Feature | WuffAgent usage | Where in WuffAgent |
|---|---|---|
| `POST /v1/chat/completions` (streaming + non-streaming) | Primary LLM call endpoint — all chat, agent, judge, memory, improvement calls | `client/chat.rs` (`stream_message`, `send_message`), `client/http.rs` (`build_request`, `build_stream_request`) |
| `GET /props` | Read `n_ctx` from `default_generation_settings` to calibrate the context window for trimming | `client/http.rs::parse_props_n_ctx` (~line 291) |
| `GET /health` | TCP probe to check server liveness (no HTTP health endpoint used) | `server/mod.rs` (TcpStream connect) |
| SSE streaming (`stream: true`) | Token-by-token response streaming for chat + agent rounds | `client/sse.rs` (`process_sse_line`, `stream_message`) |
| `usage` object in response | Token counts (prompt_tokens, completion_tokens, total_tokens) for usage logging | `types/usage.rs` (Usage struct), `client/mod.rs::record_usage` |
| `timings` object in response | Server-reported timings (prompt_ms, predicted_ms, etc.) — deserialized but not surfaced | `types/usage.rs` (timings fields) |
| `return_progress: true` | Live prompt-processing progress via `prompt_progress` stream chunks | `client/sse.rs` (PP events), `agents/agent/loop.rs` (PP callback) |
| `exceed_context_size_error` (HTTP 400) | Parsed to get prompt size + context window for force-trim retry | `client/mod.rs::parse_context_overflow`, `client/trimming.rs` |
| `stop` sequences | Request option for stopping generation | `client/http.rs` (ChatRequest struct) |
| `model` field | Model name in request (required by llama.cpp) | `client/http.rs` (ChatRequest struct) |
| `stream_options.include_usage` | Request usage in the final SSE chunk | `client/http.rs` (ChatRequest struct) |
| `enable_thinking` / `reasoning_effort` | Qwen3 thinking control via `chat_template_kwargs` | `client/http.rs` (ChatRequest struct), `types/usage.rs` (ReasoningEffort) |
| `tool_calls` (OpenAI-style) | Native tool calling in agent rounds | `client/http.rs` (ChatRequest struct), `client/sse.rs` (ToolCallTracker) |
| `POST /detokenize` | Not used (WuffAgent doesn't do token-level operations) | — |
| `POST /tokenize` | Not used (WuffAgent estimates tokens via char count) | — |
| `POST /v1/chat/completions/input_tokens` | Not used (WuffAgent estimates tokens via char count) | — |
| `GET /slots` | Not used (could be used for slot status / parallelism awareness) | — |
| `GET /metrics` | Not used (could be used for server throughput monitoring) | — |
| `POST /v1/responses/input_tokens` | Not used | — |
| `POST /v1/embeddings` | Not used (WuffAgent doesn't do embeddings) | — |
| `POST /apply-template` | Not used (WuffAgent relies on the server's template) | — |
| `POST /v1/responses` | Not used (WuffAgent uses chat completions) | — |
| `POST /v1/messages` (Anthropic) | Not used | — |
| `GET /lora-adapters` | Not used | — |
| `POST /slots/{id}?action=save/restore/erase` | Not used (slot cache management) | — |
| `GET /models` (router mode) | Not used (WuffAgent manages its own model loading) | — |
| `POST /models/load`, `POST /models/unload` | Not used (WuffAgent spawns its own server) | — |
| `GET /models/sse` | Not used (router mode model events) | — |
| `POST /models` (download) | Not used (WuffAgent manages its own model files) | — |
| `DELETE /models` | Not used | — |
| `--sleep-idle-seconds` | Not used (server sleep mode) | — |
| `GET /v1/models` (OpenAI) | Not used (could be used to get model metadata like n_ctx_train) | — |
| `POST /v1/chat/completions/control` | Not used (realtime reasoning control) | — |
| `POST /infill` | Not used (code infilling) | — |
| `POST /completion` | Not used (WuffAgent uses chat completions) | — |

## What's missing / what WuffAgent needs

### A. Server lifecycle management (local mode)

WuffAgent's `server/mod.rs` (253 lines) has a `ServerManager` that spawns a llama.cpp server as a child process. It has:
- `server_path`, `model_path`, `port`, `n_gpu_layers`, `n_ctx`, `threads`
- `tokio::process::Command` to spawn the server
- `TcpStream` probe for liveness
- `tokio Mutex` for state
- `mod progress; pub use progress::parse_progress` for loading progress

**What's missing from the server manager:**

1. **Server arguments are minimal.** The server is spawned with basic flags. llama.cpp supports many more:
   - `--jinja` (enabled by default in recent llama.cpp — needed for tool calling + reasoning)
   - `--reasoning-format` / `--reasoning` / `--reasoning-effort` (reasoning control)
   - `--metrics` (enable `/metrics` endpoint)
   - `--slots` (enable `/slots` endpoint — on by default)
   - `--cache-reuse` (KV cache reuse for prompt caching)
   - `--sleep-idle-seconds` (auto-sleep to free VRAM when idle)
   - `--sse-ping-interval` (SSE keepalive)
   - `--cache-prompt` (prompt caching — on by default)
   - `--api-key` (if WuffAgent wants to protect the local server)
   - `--parallel` / `-np` (number of slots for concurrent requests)
   - `--threads-http` (HTTP worker threads)
   - `--spec-draft-*` (speculative decoding)
   - `--lora` (LoRA adapters)
   - `--embedding` / `--rerank` (if WuffAgent ever needs embeddings)
   - `--media-path` (for local file access in multimodal)
   - `--props` (enable POST /props for runtime property changes)
   - `--reasoning-budget` (token budget for thinking)
   - `--reasoning-preserve` (preserve reasoning in history)

2. **No graceful shutdown.** The server is killed on exit, but there's no graceful drain (wait for in-flight requests to complete).

3. **No server log capture.** The server's stdout/stderr is not captured or surfaced to the user. WuffAgent has `parse_progress` for loading progress, but no general log forwarding.

4. **No automatic restart.** If the server crashes, WuffAgent doesn't restart it.

5. **No port collision detection.** If port 8080 is already in use, WuffAgent doesn't detect or handle it gracefully.

### B. Token counting (the biggest gap)

WuffAgent estimates tokens via `chars / 3.5` (calibrated from server-reported `usage.prompt_tokens`). This is a **heuristic** — it doesn't use the model's actual tokenizer.

**What's available in llama.cpp but not used:**

1. **`POST /tokenize`** — returns the actual token IDs for a string. WuffAgent could use this to get exact token counts.
2. **`POST /detokenize`** — inverse of tokenize.
3. **`POST /v1/chat/completions/input_tokens`** — counts tokens in a chat completion body without generating. This is the most direct replacement for the char-count heuristic.
4. **`POST /v1/responses/input_tokens`** — same for Responses API.
5. **`POST /v1/messages/count_tokens`** — same for Anthropic API.

**Why this matters:**
- Context trimming is based on the estimated token count. A bad estimate means either premature trimming (wasting context) or the server rejecting the request with `exceed_context_size_error` (wasting a round).
- The `exceed_context_size_error` retry path already exists and works, but it's a fallback — the primary path should be accurate.
- For models with unusual tokenizers (e.g. Qwen3, which WuffAgent explicitly supports), the 3.5 chars/token assumption is less reliable.

**Plan:**
- Add a `count_tokens` method to the `LlmClient` trait (or as a separate `TokenCounter` trait) that calls `POST /v1/chat/completions/input_tokens`.
- Use it in the trimming path to get an exact count before sending, instead of the char-count estimate.
- Keep the char-count estimate as a fast fallback for when the server doesn't support the endpoint (older llama.cpp versions).
- The `calibrate_from_usage` mechanism can stay as a secondary calibration for the estimate, but the primary path should use exact counts.

### C. Live server status (slots + metrics)

WuffAgent has no visibility into what the server is doing between requests.

**What's available:**

1. **`GET /slots`** — returns per-slot state: `is_processing`, `n_ctx`, `id_task`, `params` (including `timings_per_token`), `next_token` (including `n_decoded`). This tells WuffAgent:
   - Is a slot busy? (for concurrency awareness)
   - How many tokens have been decoded in the current request? (live TG progress)
   - What's the slot's context usage?

2. **`GET /metrics`** — Prometheus-format metrics (requires `--metrics` flag):
   - `llamacpp:prompt_tokens_total` (counter)
   - `llamacpp:prompt_tokens_seconds` (gauge: avg prompt t/s)
   - `llamacpp:tokens_predicted_total` (counter)
   - `llamacpp:predicted_tokens_seconds` (gauge: avg gen t/s)
   - `llamacpp:requests_processing` (gauge)
   - `llamacpp:requests_deferred` (gauge)
   - `llamacpp:n_tokens_max` (gauge: high watermark of context size)
   - `llamacpp:spec_decode_*` (speculative decoding stats)

3. **`GET /v1/models`** — model metadata: `n_vocab`, `n_ctx_train`, `n_embd`, `n_params`, `size`. Useful for:
   - Displaying model info in the UI
   - Knowing the training context size (vs the server's configured `n_ctx`)
   - Calculating model size for the UI

**Plan:**
- Add a `ServerStatus` struct in `wuffagent-core` that wraps a `reqwest::Client` and can poll `/slots`, `/metrics`, `/v1/models`.
- Add a `ServerMonitor` task in the bootstrap that polls these endpoints at a low frequency (e.g. every 2-5 seconds when a session is active, less often when idle).
- Surface the data in the UI status bar (extend the existing ActivityTracker plan) or in a dedicated "Server" panel.
- Use `/v1/models` to get `n_ctx_train` and display it alongside the server's configured `n_ctx` (so the user can see if they're limiting the context).

### D. Reasoning control

WuffAgent has `ReasoningEffort` (off/low/medium/high) and `enable_thinking` for Qwen3. llama.cpp supports more:

1. **`reasoning_control: true`** on the request + `POST /v1/chat/completions/control` with `action: "reasoning_end"` to force-end the reasoning block in-flight. This is useful when the model is thinking too long and the user wants to interrupt.

2. **`reasoning_budget`** — token budget for thinking (-1 = unrestricted, 0 = no thinking, N > 0 = cap). WuffAgent could expose this as a setting.

3. **`reasoning_format`** — `none` / `deepseek` / `deepseek-legacy` / auto. WuffAgent currently handles reasoning via `strip_think_tags` on the client side. Using the server's `reasoning_format: deepseek` would let the server parse and return `reasoning_content` separately, which is cleaner.

**Plan:**
- Add `reasoning_budget` to the `Config` struct (optional, default -1).
- Add a `reasoning_format` setting (default: auto).
- Wire `reasoning_control: true` into the chat completion request when the user has a "stop thinking" button in the UI.
- Use the server's `reasoning_content` field instead of `strip_think_tags` when `reasoning_format: deepseek` is in use.

### E. Prompt caching / KV cache management

llama.cpp has prompt caching (`--cache-prompt`, on by default) and KV cache reuse (`--cache-reuse`). WuffAgent doesn't control these.

**What's available:**

1. **`--cache-reuse N`** — minimum chunk size for KV cache reuse via shifting. Setting this to a reasonable value (e.g. 64) can significantly speed up repeated prompts (e.g. system prompt + tools that are the same every round).

2. **`POST /slots/{id}?action=save/restore/erase`** — save/restore/erase the KV cache of a slot. This could be used to:
   - Save the cache after a long context is built up (e.g. after a complex agent run) and restore it in a new session.
   - Erase the cache when switching models.

3. **`cache_n` in the `timings` response** — the number of prompt tokens reused from cache. WuffAgent could display this to show the user how much of the prompt was cached (and therefore how much faster the request was).

**Plan:**
- Add `cache_reuse` to the server config (optional, default 0 = server default).
- Display `cache_n` / `prompt_n` from the `timings` response in the status bar or a tooltip (e.g. "1240 cached / 52 new").
- (Later) Add a "save/restore cache" feature for long-running agent sessions.

### F. Concurrency (parallel slots)

WuffAgent sends requests sequentially (one at a time per session, and sessions run sequentially). llama.cpp supports `--parallel N` for concurrent slots.

**What's available:**
- `--parallel N` / `-np N` — number of concurrent slots.
- `GET /slots` — query slot state.
- `POST /v1/chat/completions` with a specific `model` field (in router mode) or just fire-and-forget.

**Why this matters:**
- WuffAgent already runs background LLM calls (judge, improvement, memory, eval) concurrently with the main agent round. If the server only has 1 slot, these calls queue up.
- With `--parallel 2` or more, the judge can run in a separate slot while the main agent is in slot 0.

**Plan:**
- Add `parallel` to the server config (optional, default 1).
- The server manager should pass `--parallel N` when spawning.
- (Later) The `ServerMonitor` could report slot occupancy to the UI.

### G. Model management (router mode)

llama.cpp's router mode allows loading/unloading models dynamically. WuffAgent currently manages its own model files and spawns a single-model server.

**What's available:**
- `GET /models` — list models with status (loaded/loading/unloaded/downloading).
- `POST /models/load`, `POST /models/unload` — load/unload a model.
- `POST /models` — trigger a download.
- `DELETE /models` — delete from cache.
- `GET /models/sse` — real-time model events.

**Why this matters:**
- WuffAgent could support model switching without restarting the server.
- WuffAgent could display model download progress in the UI.
- WuffAgent could manage a model cache.

**Plan:**
- (Phase 2, lower priority) Add a `ModelManager` that uses the router mode endpoints.
- Add a model selector to the UI that lists available models and allows loading/unloading.
- (Later) Add a model download feature that uses `POST /models` + `/models/sse` for progress.

### H. Error handling improvements

WuffAgent currently handles:
- `exceed_context_size_error` (HTTP 400) — force-trim and retry.
- General HTTP errors — surfaced as `Error::Http`.

**What's available in llama.cpp:**
- OAI-style error format: `{"error": {"code": 401, "message": "Invalid API Key", "type": "authentication_error"}}`.
- Custom error types: `not_supported_error` (501), `invalid_request_error` (400).

**Plan:**
- Parse the OAI error format into a structured `LlmError` type (code, message, error_type) instead of a raw string.
- Add specific handling for common error types:
  - `authentication_error` (401): show a clear "API key invalid" message.
  - `not_supported_error` (501): show "server doesn't support this feature" (e.g. if `/metrics` is disabled).
  - `invalid_request_error` (400): show the message (e.g. "Failed to parse grammar").
- (Later) Add retry logic for transient errors (503, connection reset).

### I. Server health check

WuffAgent uses a TCP probe to check if the server is running. llama.cpp has `GET /health` which returns a more informative response.

**Plan:**
- Use `GET /health` instead of (or in addition to) the TCP probe.
- The health endpoint doesn't reset the idle timer (useful with `--sleep-idle-seconds`).
- (Later) Add a health check to the `ServerMonitor` that polls every N seconds and alerts the user if the server goes down.

### J. Server log forwarding

The server's stdout/stderr (loading progress, errors, warnings) is captured by `parse_progress` for loading progress, but not forwarded to the user or the WuffAgent log.

**Plan:**
- Capture the server's stdout/stderr and forward it to the `tracing` log (at appropriate levels: INFO for loading progress, WARN for warnings, ERROR for errors).
- (Later) Add a "Server Logs" panel in the UI that shows the last N lines.

## Phases

### Phase 1: Token counting + server status (high value, low risk) — status half DONE 2026-07-19 (token counting deferred to Phase 2)

1. **Add `count_tokens` to the LlmClient trait** (or a separate `TokenCounter`):
   - `async fn count_tokens(&self, body: &ChatRequest) -> Result<u32, LlmError>`
   - Calls `POST /v1/chat/completions/input_tokens`.
   - Falls back to the char-count estimate if the server returns 404 or 501.

2. **Use exact token count in the trimming path:**
   - In `agents/agent/loop.rs`, before the LLM call, call `count_tokens` with the full message list.
   - Use the returned count (instead of `message_char_count / chars_per_token`) for the trim decision.
   - Keep the char-count estimate as the fast path when `count_tokens` is unavailable.

3. **Add `ServerStatus` + `ServerMonitor`:**
   - New module `wuffagent-core/src/server/status.rs` (or extend `server/mod.rs`).
   - `ServerStatus { slots: Vec<SlotInfo>, metrics: Option<MetricsInfo>, models: Option<ModelInfo> }`.
   - `ServerMonitor` task: polls `/slots` + `/metrics` + `/v1/models` every 3s when active.
   - Emits `AppEvent::ServerStatus { status: ServerStatus }` to the UI.

4. **Wire into bootstrap:**
   - Create the `ServerMonitor` after the server starts.
   - Add the `ServerStatus` event to the event handler.
   - Display basic server info (model name, context size, slot status) in the status bar.

**Verify:** `cargo test -p wuffagent-core server status count_tokens`
**Verify:** Manual test — start a chat, check that the status bar shows server info.

**As-built (2026-07-19):**
- `wuffagent-core/src/server/status.rs` — `poll_server_status` (GET `/slots` + `/props`, 2s connect / 5s total timeouts), `spawn_server_monitor` (tokio task, 3s interval, `Arc<AtomicBool>` active flag; emits only while reachable; stops when the event receiver drops).
- `ServerStatusInfo { reachable, slots, model, n_ctx }` + `busy_slots()`, and `SlotInfo { id, is_processing, n_ctx }` in `types/usage.rs`. Field names verified against llama.cpp b11126 `tools/server/server-context.cpp` (`/slots` returns `id`, `n_ctx`, `speculative`, `is_processing`).
- `AppEvent::ServerStatus { status }` (types/events.rs) → egui event_handler stores a shared `Arc<Mutex<ServerStatusInfo>>` (AppContext, bootstrap.rs) → status bar shows slot utilization + n_ctx + model.
- `ServerManager::start_status_monitor` / `stop_status_monitor` (monitor_active + monitor_handle fields); started from bootstrap when the server is ready and from the UI server-management flow after `wait_for_ready`.
- Tests in `server/tests.rs`; 851 lib tests pass, build clean.
- **Deferred to Phase 2:** items 1-2 (count_tokens + trimming path); `/metrics` + `/v1/models` polling (monitor polls `/slots` + `/props` only).
- **Cosmetic follow-up:** `test_slot_info_deserialization` uses `"state": "busy"` (not a real field) — rewrite the fixture with `"is_processing": true`.

### Phase 2: Server config + lifecycle + exact token counting

Code-anchored (verified 2026-07-19):
- `LocalConfig` (config/local.rs) = server_path / model_path / port / n_gpu_layers / n_ctx / threads; `Config` mirrors the fields (config/mod.rs:67); presets compare the local fields (config/presets.rs:62).
- `ServerManager::new` takes 6 positional args; `start_server_with_paths` builds the arg vec (server/mod.rs:137-150: `--model --port --host --threads --n-gpu-layers --n-ctx`); stdout+stderr are already piped (server/mod.rs:156-157) and loading progress is parsed by `progress.rs`.
- bootstrap.rs:123 constructs the `ServerManager` from `Config`.
- llama-server treats SIGINT/SIGTERM as graceful shutdown (b11126 `tools/server/server.cpp:501-502`).

1. **New server flags (config → spawn args):** ✅ done 2026-07-19 (commit 6daf523)
   - Add to `LocalConfig` (each `#[serde(default)]` so old config files load unchanged): `parallel: u32` (default 1), `cache_reuse: u32` (0 = off), `sleep_idle_seconds: i32` (-1 = off), `metrics: bool` (false), `sse_ping_interval: i32` (-1 = server default 30), `api_key: String` ("" = off). Mirror into `Config` (config/mod.rs).
   - `ServerManager::new` stops taking positional args — take the config struct instead. Args builder in `start_server_with_paths`: always `--parallel`; `--metrics` iff true; `--cache-reuse` iff > 0; `--sleep-idle-seconds` iff >= 0; `--sse-ping-interval` iff >= 0; `--api-key` iff non-empty.
   - bootstrap.rs:123 passes the config; when `api_key` is set, hand it to the ChatClient and the status monitor (both already accept an api_key parameter).
   - UI settings panel: expose `parallel` + `metrics` now (the rest stay config-file-only until needed), with a "requires server restart" hint.
   - presets.rs: leave presets as-is (new fields default; not part of preset comparison).
2. **Server log forwarding:** ✅ done (pre-existing: every stdout line forwarded at INFO with a `[llama-server]` prefix — server/mod.rs; `parse_progress` still emits the loading-% AppEvent).
3. **Port collision → attach mode:** before spawning, TCP-probe 127.0.0.1:port (the existing probe in server/mod.rs). If reachable: do NOT spawn — mark running/ready ("attached"), start the status monitor, log `attached to existing server on :{port}`. If not reachable: spawn as today. (bootstrap.rs already handles the "server already running at startup" case; this generalizes it.)
4. **Graceful shutdown (Windows):** spawn with `CREATE_NEW_PROCESS_GROUP` (tokio `creation_flags`); on stop: `GenerateConsoleCtrlEvent(CTRL_C_EVENT, <group>)` via `windows-sys` (new dep, windows target only) → wait up to 5s for the child to exit → `kill()` fallback. Keep `kill()` when the flags are unavailable (documented).
5. **Exact token counting (Phase 1 remainder, items 1-2):** ✅ done 2026-07-19 (commit f40d022)
   - `client/http.rs`: `count_input_tokens(http_client, base_url, api_key, request: &ChatRequest) -> Result<u32, Error>` → `POST /v1/chat/completions/input_tokens`, parse `{"count": N}`.
   - Trimming path (agents/agent/loop.rs ~line 297): call it ONLY when the char-count estimate lands within ±5% of `trim_trigger_chars` (otherwise no extra HTTP round-trip); the exact count decides the trim.
   - Feature-detect once per client: 404/501 → `input_tokens_supported = false`, permanently fall back to the char estimate. `calibrate_from_usage` stays as secondary.
6. **(Optional) `/metrics` + `/v1/models` polling:** ✅ done 2026-07-19
   - `server/status.rs`: `parse_metrics_text` (small Prometheus text subset — `llamacpp:prompt_tokens_seconds`, `llamacpp:predicted_tokens_seconds`, `llamacpp:requests_processing`, `llamacpp:n_tokens_max`; metric names verified against b11126 `tools/server/server-task.cpp` `to_metrics`), `n_ctx_train_from_models` (b11126 shape `{"data":[{meta:{n_ctx_train}}]}`, tolerant of bare array / top-level field), `fetch_n_ctx_train` (one-shot GET).
   - `poll_server_status(base_url, api_key, metrics_enabled)` — `GET /metrics` only when the server was started with `--metrics` (the endpoint 405s otherwise → `metrics: None`).
   - `spawn_server_monitor(..., metrics_enabled)` — fetches `/v1/models` ONCE per monitor lifetime, keeps `n_ctx_train` across snapshots.
   - `types/usage.rs`: `ServerMetrics` (4 fields) + `ServerStatusInfo.{metrics, n_ctx_train}`.
   - Status-bar pill (egui `ui/status.rs`): `⚙ Slots 0/1 · n_ctx 4096 / 32768 · <model> · 18 t/s`, full tooltip with per-stage t/s, requests processing, largest observed sequence.

**Verify:** `cargo test -p wuffagent-core config server client` + manual: inspect the spawned process's command line (new flags present), server log lines in the WuffAgent log, attach mode when the port is busy, graceful stop (server exits within 5s without a kill).

### Phase 3: Prompt caching display + reasoning tuning (verified against llama.cpp b11126 @ M:\repos\llama.cpp e167182)

Wire facts (verified in the local llama.cpp source):
- The `timings` response object carries `cache_n`, `prompt_n`, `prompt_ms`, `predicted_n`, `predicted_ms` in addition to the `*_per_second` speeds WuffAgent already parses (tools/server/server-common.cpp:86-94). `LlamaTimings` (types/usage.rs) currently captures only the two speeds.
- Request-body fields: `reasoning_format` (string; server-schema.cpp:302, server-common.cpp:1318) and `reasoning_budget_tokens` (int, -1 = server default; server-common.cpp:1388 — note the wire name is `reasoning_budget_tokens`, not `reasoning_budget`).
- `POST /v1/chat/completions/control` (`action: "reasoning_end"`) does NOT exist in b11126 — stop-thinking needs a newer server build.
- The client already parses `reasoning_content` (`Message.reasoning_content`, `thinking_chars` in http.rs) — no new response-parsing work for reasoning text.

1. **Extend `LlamaTimings`** (types/usage.rs): add `prompt_n: Option<u32>`, `prompt_ms: Option<f64>`, `predicted_n: Option<u32>`, `predicted_ms: Option<f64>`, `cache_n: Option<u32>` — all `#[serde(default)]`, so older servers/backends deserialize unchanged.
2. **Cache display:** plumb `cache_n` + `prompt_n` wherever the final usage is surfaced (SSE final-chunk fold + http.rs non-stream fold → round-complete / record_usage path) → status-bar tooltip on the last round: "1240 cached / 52 new (96% cached)". (Optional sub-item: append `cache_n` to the usage-log line so cumulative hit-rate can be computed later.)
3. **`reasoning_budget_tokens`:** ✅ done 2026-07-19 (commit d7328f1) — `ChatRequest`/`ChatRequestRef` gained `reasoning_budget_tokens: Option<i32>` (omitted when None, golden test extended with a (format, budget) matrix arm); `build_request` takes it as a param (all 15 test call sites updated); `ChatClient` stores it clone-isolated (`set_reasoning_budget`); `Config.reasoning_budget_tokens` (serde default None) stamped in bootstrap on session / non-streaming / memory clients.
4. **`reasoning_format`:** ✅ done 2026-07-19 (commit d7328f1, with item 3) — `reasoning_format: Option<String>` on both request types (wire name `reasoning_format`); `"deepseek"` / `"deepseek-legacy"` ready for DeepSeek-family models; `strip_think_tags` remains the fallback. Test: `test_build_request_reasoning_budget_and_format`.
5. **(Later — requires a newer llama.cpp) "stop thinking" button:** feature-detect the control endpoint once (404 → hide the control); request field `reasoning_control: true` while the button exists; the streaming-state UI button sends `{"request_id": <id>, "action": "reasoning_end"}`. Parked until the server build is upgraded.

**Verify:** `cargo test -p wuffagent-core` (timings deserialization incl. a full b11126 `timings` fixture + the ChatRequest golden test) + manual with Qwen3: cache numbers visible in the tooltip, `reasoning_budget_tokens: 256` visibly caps thinking length, `reasoning_format: "deepseek"` returns clean `reasoning_content` (dimmed thinking block).

### Phase 4: Model management (router mode) — lower priority

1. **ModelManager:**
   - New module `wuffagent-core/src/server/models.rs`.
   - Uses the router mode endpoints: `GET /models`, `POST /models/load`, `POST /models/unload`, `POST /models`, `GET /models/sse`.
   - Supports model switching without server restart.

2. **UI model selector:**
   - Add a model dropdown to the settings or status bar.
   - List available models from `GET /models`.
   - Allow loading/unloading models.
   - (Later) Add a model download feature with progress.

3. **Model metadata display:**
   - Show `n_ctx_train`, `n_params`, `size` from `GET /v1/models` in the model info tooltip.

**Verify:** `cargo build` + manual test with router mode (requires a llama.cpp build with router support).

### Phase 5: Error handling + health check improvements — ✅ done

1. **Structured LlmError:** ✅
   - `LlmError { code: u16, message: String, error_type: String }` in `client/http.rs` + `Error::Llm` variant (`#[from]`) in `client/mod.rs`; `llm_error_or_http()` builds it from any non-success response (falls back to the legacy `Error::Http` string for non-JSON bodies).
   - Specific handling: `LlmError::context_overflow()` reads `n_prompt_tokens`/`n_ctx` from the envelope and `parse_context_overflow()` now branches on the structured variant (string parser kept for `Error::Http`).
   - Unit tests: overflow envelope, 503 retryability, non-JSON fallback, `error`-string shape, missing-tokens edge.

2. **Health check:** ✅
   - `poll_server_status` probes `GET /health` first (200 OR 503 ⇒ reachable — the most reliable liveness signal; `/slots`/`/props` may 404/503 on some builds).
   - The monitor now emits on the UP→DOWN and DOWN→UP transition edges (plus periodic snapshots while up); the status bar shows a red "⚙ server down" pill (with last-known model in the tooltip) on the down edge — there is no toast system in the app, the pill IS the notification. `ServerStatusInfo` gained `base_url` (was never set before).

3. **Retry logic:** ✅
   - `send_with_transient_retries()` in `client/http.rs`: exponential backoff 250ms/500ms/1s (3 retries) for connection errors (`is_connect`) and 502/503/504; any other result (success, 4xx, other 5xx) is returned immediately so callers keep their existing behavior (400 overflow retry path untouched). Cancellation-aware for the streaming path (`tokio::select!` on the backoff sleep).
   - Wired into BOTH `send_message` (non-stream) and the streaming `chat` request.
   - Unit tests: 503,503→200 succeeds; 400 not retried; exhausted retries report structured 503; connection-refused retried then reported.

**Verify:** `cargo test -p wuffagent-core` — ✅ 887 + 2 + 1 passed, 0 failed (2026-10-02); `cargo check -p wuffagent-egui` clean. Manual server-error test pending (needs the llama.cpp server running).

## Out of scope

- **Embeddings / reranking** — WuffAgent doesn't do embeddings. If needed in the future, add `--embedding` to the server config and a new `EmbeddingClient`.
- **Code infilling** — `POST /infill` is not relevant for WuffAgent's use case.
- **LoRA adapters** — `GET /lora-adapters` + `POST /lora-adapters` could be used later for model fine-tuning, but not in the current scope.
- **Multimodal** — WuffAgent's `show_image` tool sends image data via the `show_image` tool, not via the LLM request. Multimodal support in the LLM request (image_url in messages) could be added later.
- **Speculative decoding** — `--spec-draft-*` flags are performance optimizations that don't require client-side changes. They can be added to the server config in Phase 2 if the user wants.

## Risks / notes

- **`count_tokens` endpoint availability:** The `POST /v1/chat/completions/input_tokens` endpoint is relatively new in llama.cpp. Older versions may not have it. The fallback to char-count estimation is essential.
- **Router mode requires a specific build:** The router mode (dynamic model loading) requires a llama.cpp build with router support. Not all builds have it. The `ModelManager` should degrade gracefully (fall back to single-model mode).
- **Server config changes require restart:** Most server flags (e.g. `--parallel`, `--metrics`) are set at startup. Changing them requires a server restart. WuffAgent should show a "restart server" button when the user changes a server config that requires restart.
- **Concurrency with parallel slots:** If the server has multiple slots, the `GET /slots` response will have multiple entries. The `ServerMonitor` should aggregate them (e.g. "2/4 slots busy").
- **SSE keepalive:** llama.cpp sends SSE pings every 30 seconds by default (`--sse-ping-interval`). WuffAgent's `read_timeout` (5 minutes) is much longer, so the pings are not strictly necessary, but they help detect dead connections faster.

## Next step

Implement Phase 2, one item per turn (commit after each):
1. `LocalConfig`/`Config` new fields + `ServerManager` config-driven args builder + bootstrap wiring.
2. Server log forwarding (stdout read loop → `tracing::debug!(target: "llama-server")`).
3. Port-collision attach mode.
4. Graceful shutdown (CREATE_NEW_PROCESS_GROUP + CTRL_C, 5s wait, kill fallback).
5. `count_input_tokens` + trimming-path use (±5% gate) + one-shot feature detect.
6. Optional: `/metrics` + `/v1/models` in the status monitor.
Then Phase 3 in order: `LlamaTimings` fields → cache display → `reasoning_budget_tokens` + `reasoning_format` wire fields → (parked) stop-thinking.
