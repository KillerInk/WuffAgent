//! Tiny append-only log: `~/.wuffagent/telegram.log` (also mirrored to
//! stderr). Rotates to `telegram.log.old` past 1 MB.
//!
//! Keep per-line cost low: open-append per line is fine at bot frequency.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_FILE: &str = "telegram.log";
const MAX_BYTES: u64 = 1024 * 1024;

fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Human-ish timestamp (UTC, no deps): days since epoch -> not calendar,
    // just a monotonic counter is enough for correlating lines.
    format!("{secs}")
}

pub fn log(dir: &Path, msg: &str) {
    eprintln!("[telegram] {msg}");
    let path = dir.join(LOG_FILE);
    let _ = std::fs::create_dir_all(dir);
    // Rotate first if needed (best effort).
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_BYTES {
            let _ = std::fs::rename(&path, dir.join(format!("{LOG_FILE}.old")));
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {msg}", now());
    }
}
