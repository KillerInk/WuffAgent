//! Cross-directory prompt-history helpers for the agent editor and the
//! improvements panel (F4 UI).
//!
//! `AgentManager` keeps snapshots in `<primary>/history/`. Because the F3
//! path-aware approve writes a profile into the directory it ACTUALLY lives
//! in (a search dir, not always the primary), an agent's snapshots can sit
//! in several known agents directories. These helpers merge those lists and
//! route a revert through a manager bound to the snapshot's own directory.

use std::path::{Path, PathBuf};

use crate::agents::config::{AgentConfig, AgentManager};

/// One prompt-history snapshot, tagged with the agents directory that owns
/// it (a revert must go through a manager bound to that directory).
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// Agents directory containing the snapshot (its `history/` subdir).
    pub dir: PathBuf,
    /// Path to the snapshot file.
    pub path: PathBuf,
    /// Unix timestamp parsed from the filename (0 = unparseable).
    pub ts: u64,
    /// Same-second sequence suffix (0 = base name).
    pub seq: u32,
}

/// Parse `(unix_ts, seq)` out of a snapshot-filename tail — the part after
/// the `<name>-` prefix, i.e. `<unixts>` or `<unixts>-<seq>`. Mirrors
/// `AgentManager::history_file_order`; non-numeric parts yield 0.
pub fn parse_ts_seq(tail: &str) -> (u64, u32) {
    let mut parts = tail.splitn(2, '-');
    let ts = parts.next().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
    let seq = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    (ts, seq)
}

/// Format a unix timestamp for display (UTC, e.g. `2026-07-10 14:05 UTC`).
pub fn format_ts(ts: u64) -> String {
    use chrono::{TimeZone, Utc};
    match Utc.timestamp_opt(ts as i64, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => format!("timestamp {}", ts),
    }
}

/// List the prompt-history snapshots of `name` across ALL of `dirs`
/// (ordered, primary first), newest first.
///
/// `dirs` should be the same discovery set the agent selector uses. An empty
/// result means the agent has no history anywhere — not an error.
pub fn list_history(dirs: &[PathBuf], name: &str) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = Vec::new();
    let prefix = format!("{}-", name);
    for dir in dirs {
        let mgr = AgentManager::new(dir.clone());
        let Ok(snaps) = mgr.list_agent_history(name) else {
            continue;
        };
        for path in snaps {
            if entries.iter().any(|e| e.path == path) {
                continue; // same dir listed twice
            }
            let file_name = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or_default()
                .to_string();
            let stem = file_name.strip_suffix(".json").unwrap_or(&file_name);
            let tail = stem.strip_prefix(&prefix).unwrap_or("");
            let (ts, seq) = parse_ts_seq(tail);
            if ts > 0 {
                entries.push(HistoryEntry {
                    dir: dir.clone(),
                    path,
                    ts,
                    seq,
                });
            }
        }
    }
    entries.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.seq.cmp(&a.seq)));
    entries
}

/// Restore `name` from `entry` via a manager bound to `dir` (normally
/// `entry.dir`). Returns the restored config, and the revert itself snapshots
/// the current file first so it is reversible.
pub fn revert(
    dir: &Path,
    name: &str,
    entry: &HistoryEntry,
) -> Result<AgentConfig, crate::agents::AgentError> {
    AgentManager::new(dir.to_path_buf()).revert_agent(name, &entry.path)
}

/// Single-line, truncated preview of a snapshot's system prompt for the
/// history list (handles both `AgentConfig` and legacy `WorkerConfig` files).
pub fn prompt_preview(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return "(unreadable)".to_string();
    };
    let prompt = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v.get("system_prompt").and_then(|s| s.as_str()).map(str::to_string))
        .unwrap_or_default();
    let flat: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= 60 {
        flat
    } else {
        format!("{}...", chars.iter().take(57).collect::<String>())
    }
}

#[cfg(test)]
mod tests;
