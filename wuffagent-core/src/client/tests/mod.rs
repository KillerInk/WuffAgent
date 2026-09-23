//! Unit tests for the `client` module (see `super`), split by area:
//! - `request.rs`: request building, calibration/trim, wire parsing, TPS guards
//! - `sse.rs`: SSE line processing + tool-call warnings
//! - `ready.rs`: early tool-call ("ready") detection
//! - `usage.rs`: usage.jsonl logging for a completed call

mod ready;
mod request;
mod sse;
mod usage;

use crate::types::ToolCall;
use super::*;
