//! Token-usage and server-timing wire types.

use serde::Deserialize;

/// Token usage statistics returned by the API.
#[derive(Deserialize, Debug, Clone)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// llama.cpp extension: server-reported per-stage speeds for the
    /// completed call. The server sends these in a `timings` field that is a
    /// SIBLING of `usage` on the wire; the client folds them in here (see
    /// `http.rs` / `sse.rs`) so the UI can show tokens/sec. `None` for
    /// backends that don't report timings.
    #[serde(default)]
    pub timings: Option<LlamaTimings>,
}

/// Live prompt-processing progress from the llama.cpp server: a
/// `prompt_progress` object in stream chunks (requested via
/// `return_progress: true` in the request body). Sent per server main-loop
/// tick while the prompt is being processed.
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct PromptProgress {
    /// Total prompt tokens for this call.
    pub total: u32,
    /// Tokens served from the KV cache (processed nearly for free).
    pub cache: u32,
    /// Tokens processed so far (including cached ones).
    pub processed: u32,
    /// Milliseconds elapsed since prompt processing started.
    pub time_ms: f64,
}

impl PromptProgress {
    /// Effective prompt-processing speed (tokens/s) counting only
    /// non-cached tokens. `None` while nothing new has been processed.
    pub fn prompt_tps(&self) -> Option<f64> {
        let new = f64::from(self.processed) - f64::from(self.cache);
        if self.time_ms < 1.0 || new < 1.0 {
            return None;
        }
        Some(new / (self.time_ms / 1000.0))
    }
}

/// llama.cpp server-reported per-stage timings for one completed call.
///
/// The server sends the full `server_slot_stats` object as a `timings`
/// SIBLING of `usage` (field names verified against b11126
/// `tools/server/server-common.cpp` `server_slot_stats::to_json`). All
/// fields are optional so older server builds and other backends
/// deserialize unchanged.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct LlamaTimings {
    /// Prompt processing speed (tokens/second).
    #[serde(default)]
    pub prompt_per_second: Option<f64>,
    /// Token generation (llama.cpp "predicted") speed (tokens/second).
    #[serde(default)]
    pub predicted_per_second: Option<f64>,
    /// Prompt tokens reused from the KV cache (processed nearly for free).
    #[serde(default)]
    pub cache_n: Option<u32>,
    /// Total prompt tokens processed (cached + new).
    #[serde(default)]
    pub prompt_n: Option<u32>,
    /// Milliseconds spent processing the prompt.
    #[serde(default)]
    pub prompt_ms: Option<f64>,
    /// Number of generated tokens.
    #[serde(default)]
    pub predicted_n: Option<u32>,
    /// Milliseconds spent generating tokens.
    #[serde(default)]
    pub predicted_ms: Option<f64>,
}

impl LlamaTimings {
    /// Prompt tokens that had to be actually processed (not served from the
    /// KV cache): `prompt_n - cache_n`. `None` when the server didn't
    /// report both.
    pub fn new_prompt_tokens(&self) -> Option<u32> {
        match (self.prompt_n, self.cache_n) {
            (Some(p), Some(c)) if p > c => Some(p - c),
            (Some(p), Some(c)) if p == c => Some(0),
            _ => None,
        }
    }

    /// Fraction of the prompt served from the KV cache, 0.0..=1.0. `None`
    /// when the server didn't report the cache stats or there was no prompt.
    pub fn cache_fraction(&self) -> Option<f64> {
        match (self.prompt_n, self.cache_n) {
            (Some(p), Some(c)) if p > 0 => Some((c as f64) / (p as f64)),
            _ => None,
        }
    }
}

/// A single slot's state from llama.cpp's `GET /slots` endpoint.
///
/// The server has `--parallel N` slots; each can process one request at a
/// time. This is a subset of the server's response — only the fields
/// WuffAgent needs for status display.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SlotInfo {
    /// Slot index (0-based).
    #[serde(default)]
    pub id: u32,
    /// Whether this slot is currently processing a request.
    #[serde(default)]
    pub is_processing: bool,
    /// The slot's context window size (tokens).
    #[serde(default)]
    pub n_ctx: u32,
}

/// Prometheus gauges/counters from llama.cpp's `GET /metrics` endpoint
/// (server started with `--metrics`). Parsed by the status monitor from the
/// small Prometheus text subset the server emits — metric names verified
/// against b11126 `tools/server/server-task.cpp` (`to_metrics`), e.g.
/// `llamacpp:prompt_tokens_seconds 123.45`. `None` per field when the
/// server doesn't report it.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ServerMetrics {
    /// Prompt-processing throughput (tokens/s) — `llamacpp:prompt_tokens_seconds`.
    #[serde(default)]
    pub prompt_tps: Option<f64>,
    /// Token-generation throughput (tokens/s) — `llamacpp:predicted_tokens_seconds`.
    #[serde(default)]
    pub predicted_tps: Option<f64>,
    /// Number of requests currently being processed — `llamacpp:requests_processing`.
    #[serde(default)]
    pub requests_processing: Option<u32>,
    /// Largest observed sequence length (prompt + generation) — `llamacpp:n_tokens_max`.
    #[serde(default)]
    pub n_tokens_max: Option<u32>,
}

/// Server status snapshot from llama.cpp's `GET /slots` + `GET /props`
/// (and, when enabled/available, `GET /metrics` + `GET /v1/models`)
/// endpoints. Emitted to the UI via `AppEvent::ServerStatus`.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ServerStatusInfo {
    /// Per-slot state from `GET /slots` (empty when the endpoint is
    /// unavailable or the server is not running).
    #[serde(default)]
    pub slots: Vec<SlotInfo>,
    /// The server's configured context window (tokens), from
    /// `GET /props` → `default_generation_settings.n_ctx`.
    /// `None` when the endpoint is unavailable.
    #[serde(default)]
    pub n_ctx: Option<u32>,
    /// Model name reported by the server (from `/props` or
    /// `/v1/models`; `None` when unavailable).
    #[serde(default)]
    pub model: Option<String>,
    /// Whether the server is reachable (last probe succeeded).
    #[serde(default)]
    pub reachable: bool,
    /// Live throughput gauges from `GET /metrics` (only when the server was
    /// started with `--metrics`; `None` otherwise).
    #[serde(default)]
    pub metrics: Option<ServerMetrics>,
    /// The model's trained context window (tokens), from `GET /v1/models`
    /// → `data[0].meta.n_ctx_train` (fetched once per monitor lifetime).
    /// `None` when the endpoint is unavailable. Used for the status-bar
    /// tooltip "n_ctx 4096 / train 32768".
    #[serde(default)]
    pub n_ctx_train: Option<u32>,
}

impl ServerStatusInfo {
    /// Number of busy slots (for status-bar display, e.g. "2/4 busy").
    pub fn busy_slots(&self) -> (u32, u32) {
        let total = self.slots.len() as u32;
        let busy = self.slots.iter().filter(|s| s.is_processing).count() as u32;
        (busy, total)
    }
}
