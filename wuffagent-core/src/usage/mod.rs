//! Token-usage tracking: per-LLM-call logging and aggregation.
//!
//! [`recorder`] appends one JSONL line per completed LLM call to
//! `~/.wuffagent/usage.jsonl` (sibling of `sessions/`). [`stats`] is the
//! pure aggregation layer: tolerant JSONL loading, hour/day/week bucketing
//! with zero-filled windows, and an incremental log reader for the UI panel.
//!
//! Design notes:
//! - Writes are best-effort: a log failure must never break the chat (the
//!   recorder degrades to `tracing::warn!` and moves on).
//! - `stats` has no I/O of its own beyond reading the log file, and all
//!   bucket math is done on local *wall-clock* `NaiveDateTime`s so DST
//!   transitions can't shift bucket boundaries.

pub mod recorder;
pub mod stats;

pub use recorder::{UsageEntry, UsageRecorder};
