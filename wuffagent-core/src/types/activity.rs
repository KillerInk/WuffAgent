//! Live LLM-activity snapshot types (pure data — `types` has no internal
//! dependencies). Emitted by `crate::activity::ActivityTracker` in
//! `AppEvent::LlmActivity` so the status bar can show what is actually
//! running.

use std::time::{Duration, SystemTime};

/// The phase an in-flight LLM call is currently in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ActivityPhase {
    /// Between the call starting and the first streamed event (the server
    /// has reported neither prompt progress nor a token yet).
    Thinking,
    /// Prompt processing (llama.cpp `prompt_progress`): `processed` of
    /// `total` tokens ingested, `time_ms` elapsed in this phase.
    PromptProcessing {
        processed: u32,
        total: u32,
        time_ms: f64,
    },
    /// Token generation in flight.
    Generating,
}

impl ActivityPhase {
    /// One-line human-readable phase for status-bar labels. For
    /// `PromptProcessing` this includes the processed percentage.
    pub fn describe(&self) -> String {
        match self {
            ActivityPhase::Thinking => "thinking".to_string(),
            ActivityPhase::PromptProcessing { processed, total, .. } => {
                let pct = if *total > 0 {
                    ((*processed as u64) * 100 / (*total as u64)) as u32
                } else {
                    0
                };
                format!("processing prompt… {pct}%")
            }
            ActivityPhase::Generating => "streaming".to_string(),
        }
    }
}

/// One entry of the live LLM-activity snapshot
/// (`AppEvent::LlmActivity.activities`). The snapshot is a full state
/// replacement for the UI (no incremental bookkeeping).
#[derive(Clone, Debug, PartialEq)]
pub struct LlmActivityInfo {
    /// What is running ("judge", "agent: coder", "memory", "eval: t1", …).
    pub label: String,
    /// The session this activity belongs to; `None` = background/global
    /// call (memory, improvement, …).
    pub session_id: Option<String>,
    /// Current phase (Thinking → PromptProcessing → Generating).
    pub phase: ActivityPhase,
    /// Tokens generated so far.
    pub tokens_out: u32,
    /// Speed for the current phase: PP = `PromptProgress::prompt_tps()`,
    /// TG = tokens since phase start / phase duration. `None` while too
    /// little has happened to be meaningful.
    pub tps: Option<f64>,
    /// When the call started.
    pub started_at: SystemTime,
    /// Duration so far.
    pub duration: Duration,
}
