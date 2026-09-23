//! Unit tests for the manager module (see super), split by area:
//! - core.rs: add/search/dedup/eviction/update/delete/context/query-injection
//! - maintenance.rs: LLM maintenance actions (single + batched)


use crate::types::Message;
use super::*;
use tempfile::tempdir;

mod core;
mod maintenance;

