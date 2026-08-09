# WuffAgent - Build Instructions

## Prerequisites

- Rust toolchain (Rust 1.75+): https://www.rust-lang.org/tools/install
- `cargo` must be on PATH

## Quick Start

```bash
# Clone the repository
git clone <repo-url>
cd WuffAgent

# Run checks and tests
cargo check
cargo test

# Build for development
cargo build

# Build for release
cargo build --release

# Run the application
cargo run
# or, from the release build:
target/release/wuffagent.exe
```

## Build Commands

| Command | Description |
|---------|-------------|
| `cargo check` | Fast syntax and type check (no compilation) |
| `cargo build` | Debug build with optimizations disabled |
| `cargo build --release` | Release build with full optimizations |
| `cargo test` | Run all unit and integration tests |
| `cargo test --release` | Run tests in release mode |
| `cargo doc --open` | Generate and open documentation |
| `cargo clippy` | Run the Rust linter |
| `cargo fmt` | Format all code according to Rust standards |

## Test Output

```
running 25 tests
test client::tests::test_build_request_no_system_prompt ... ok
test client::tests::test_build_request_streaming ... ok
test client::tests::test_build_request_with_system_prompt ... ok
test client::tests::test_process_sse_line_done ... ok
test client::tests::test_process_sse_line_empty ... ok
test client::tests::test_process_sse_line_empty_content ... ok
test client::tests::test_process_sse_line_non_data ... ok
test client::tests::test_process_sse_line_multiple_chunks ... ok
test client::tests::test_process_sse_line_valid_chunk ... ok
test config::tests::test_config_default ... ok
test config::tests::test_config_load_nonexistent ... ok
test config::tests::test_config_save_and_load ... ok
test config::tests::test_config_validate_empty_paths ... ok
test config::tests::test_config_validate_invalid_gpu_layers ... ok
test config::tests::test_config_validate_invalid_port ... ok
test config::tests::test_config_validate_invalid_threads ... ok
test config::tests::test_config_validate_nonexistent_paths ... ok
test config::tests::test_config_validate_valid ... ok
test server::tests::test_parse_progress ... ok
test server::tests::test_server_manager_creation ... ok
test test_chat_client_error_handling ... ok
test test_chat_client_request ... ok
test test_config_persistence ... ok
test test_server_manager_state ... ok
test test_sse_parsing_end_to_end ... ok

test result: ok. 25 passed; 0 failed; 0 ignored
```

## Output

- Debug binary: `target/debug/wuffagent.exe`
- Release binary: `target/release/wuffagent.exe`
