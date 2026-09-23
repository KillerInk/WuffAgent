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

/// llama.cpp server-reported per-stage speeds for one completed call.
#[derive(Deserialize, Debug, Clone)]
pub struct LlamaTimings {
    /// Prompt processing speed (tokens/second).
    #[serde(default)]
    pub prompt_per_second: Option<f64>,
    /// Token generation (llama.cpp "predicted") speed (tokens/second).
    #[serde(default)]
    pub predicted_per_second: Option<f64>,
}
