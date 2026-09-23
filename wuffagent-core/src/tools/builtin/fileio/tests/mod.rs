//! Unit tests for the `fileio` module (see `super`), split per op group:
//! - `read.rs`: read_file (full, ranges, line numbers, truncation)
//! - `write.rs`: write/append/list_dir
//! - `search.rs`: search_files + path validation
//! - `diff.rs`: apply_diff (blocks, CRLF payload, anchoring)
//! - `move_copy.rs`: mkdir/delete/copy/move/file_info
//! - `wiring.rs`: named tool wiring
//! - `lineendings.rs`: CRLF / BOM / UTF-16 handling

use super::*;
use std::fs;
use std::io::Read;

mod diff;
mod lineendings;
mod move_copy;
mod read;
mod search;
mod wiring;
mod write;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wuff_file_io_{}_{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(p: &str, s: &str) {
    fs::write(p, s).unwrap();
}

fn read_string(path: &str) -> String {
    let mut buf = String::new();
    let mut file = fs::File::open(path).unwrap();
    file.read_to_string(&mut buf).unwrap();
    buf
}

fn params(pairs: &[(&str, &str)]) -> ToolParams {
    let mut m = HashMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), serde_json::json!(v));
    }
    ToolParams { values: m }
}

fn success_json(out: ToolOutput) -> serde_json::Value {
    match out {
        ToolOutput::Success(v) => v,
        other => panic!("expected Success, got {other:?}"),
    }
}
