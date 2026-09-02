# AGENTS.md

This file provides guidance to agents when working with code in this repository.

Rust 2021 Cargo workspace: `wuffagent-core` (shared lib, crate name `wuffagent_core`) + `wuffagent-egui` (eframe 0.30 GUI binary that depends on core).

## Commands

- Build: `cargo build` (all) / `cargo build -p wuffagent-core` / `cargo build -p wuffagent-egui`
- Run GUI: `cargo run -p wuffagent-egui`
- Test all: `cargo test -p wuffagent-core`
- Run a single test: `cargo test -p wuffagent-core <test_name>` (e.g. `cargo test -p wuffagent-core test_trim_conversation`)
- Filter by module: `cargo test -p wuffagent-core client::session`

## Key non-obvious facts

- **No rustfmt/clippy config** — default `cargo fmt` / `cargo clippy` conventions apply; no CI lint gate.
- **Tests are inline** in each source file as `#[cfg(test)] mod tests`; async tests use `#[tokio::test]`. `tempfile` is the only dev-dependency and tests use `std::env::temp_dir()` for isolation.
- **Config path resolution** (wuffagent-core/src/config/paths.rs): prefers `config.json` next to the executable (portable-app style), falling back to `dirs::config_dir()/wuffagent/config.json`. Sessions live in a sibling `sessions/` dir; agents in sibling `agents/`; memories in `<config_dir>/wuffagent/memories/projects/<project>.json`.
- **Agent discovery order** (wuffagent-egui/src/main.rs): config-dir `agents/`, then exe-dir `../agents`, then cwd `agents/`. First-seen name wins dedup. `AgentManager` writes only to the primary (config) dir, never to search dirs.
- **Agent config migration**: `AgentRegistry::load_agent_config` and `Config` deserialization both silently migrate legacy `WorkerConfig` / inline `agent_config` JSON to the new `AgentConfig` format (writes `agents/general.json` as a side effect during config load).
- **Plugin tool ABI** (wuffagent-core/src/tools/dynamic/loader.rs): plugins are native libs exporting `wuff_tool_metadata` and `wuff_tool_create` C symbols, loaded via libloading from `<config_dir>/wuffagent/plugins`.
- **Reasoning effort wire mapping** (wuffagent-core/src/types.rs): `ReasoningEffort::High` serializes to `"xhigh"` on the wire, and `Off` is omitted from requests entirely.
- **Session file format**: plain JSON, or base64 text prefixed with `WUFFENC` magic bytes (7 bytes + 12-byte nonce + ChaCha20Poly1305 ciphertext). `load_session` returns `None` for encrypted files — callers must use `decrypt_and_load_session`.
- **Memory file writes are atomic** via temp-file + rename (`wuffagent-core/src/memory/storage.rs`); keep this pattern for any new file persistence.
- **Module dependency rules** documented in wuffagent-core/src/lib.rs: `types` has no internal deps; no circular deps between top-level modules. `config_types` mirrors `config` re-exports for backward compatibility — don't delete it.
- **egui `main.rs` re-exports core modules** (`pub use wuffagent_core::{...}`) so `wuffagent-egui/src/ui/*` can use `crate::...` paths instead of full `wuffagent_core::` paths.
