//! Freshness-aware invalidation of stale file-read results.
//!
//! The size/age-based trimmer knows nothing about whether its content is still
//! true. That is wrong for `read_file` results: once the agent edits a file,
//! every earlier snapshot of it is stale — kept in context it actively misleads
//! (`apply_diff` SEARCH blocks stop matching, the model reasons about deleted
//! lines) while eating budget that still-relevant context could use.
//!
//! The stale state is derived at trim time as a **pure function of the message
//! list** (see [`build_file_state_index`]) instead of being tracked in per-agent
//! state:
//!
//! - **Resume-safe** — the per-turn list is rebuilt from the persisted session
//!   store each turn, so a per-run map would be empty (and blind) after a
//!   reload; derivation needs no replay logic.
//! - **Shift-safe** — removals during trimming shift indices; re-deriving on
//!   the current list per trim is trivially correct.
//!
//! For every pre-tail `read_file` result at index `i` of path `P`:
//!
//! | Condition | Verdict |
//! |---|---|
//! | a successful mutation of `P` at an index `> i` | **stale** — pair removed |
//! | else a newer read of `P` (by construction no mutation lies between them) | **superseded** — pair removed |
//! | else | **current** — newest snapshot, nothing mutated since; kept in full |
//!
//! Stale/superseded reads are removed WHOLESALE (the assistant tool call and
//! its tool results go together) rather than being replaced by a one-line
//! marker: even a marker keeps the model believing it has seen the file, and
//! it then hallucinates line contents it no longer has. Removal makes the
//! absence explicit — if the file matters, the model re-reads it.

use std::collections::HashMap;

use crate::tools::manager::parse_tool_args;
use crate::tools::types::ToolParams;
use crate::types::Message;

/// Marker prefix shared by both freshness markers. Messages already carrying
/// it are skipped, which makes repeated trims idempotent.
const MARKER_PREFIX: &str = "[read_file of ";

/// A tool result whose content starts with this prefix is a failure (the agent
/// loop formats failures exactly this way) and must not count as a mutation —
/// a failed `write_file` did not change the file.
const FAILURE_PREFIX: &str = "Error:";

/// Stale marker: a mutation of the file happened after this read.
fn stale_marker(path: &str) -> String {
    format!(
        "{MARKER_PREFIX}{path} — stale: file modified after this read; re-read before relying on it]"
    )
}

/// Superseded marker: a newer read of the same file exists and no mutation
/// lies between them, so the newer read's content is the current one.
fn superseded_marker(path: &str) -> String {
    format!("{MARKER_PREFIX}{path} — superseded by a newer read of the same file]")
}

/// True if `content` is already a freshness marker (idempotency guard).
pub fn is_file_marker(content: &str) -> bool {
    content.starts_with(MARKER_PREFIX)
}

/// Normalize a file path for use as a stale-index key.
///
/// Trims whitespace, unifies separators to `/`, strips leading `./`, collapses
/// duplicate separators, and case-folds on Windows (paths are case-insensitive
/// there). Applied at both record and lookup time.
pub fn normalize_path(path: &str) -> String {
    let mut p: String = path.trim().replace('\\', "/");
    while let Some(rest) = p.strip_prefix("./") {
        p = rest.to_string();
    }
    let mut collapsed = String::with_capacity(p.len());
    for c in p.chars() {
        if c == '/' && collapsed.ends_with('/') {
            continue;
        }
        collapsed.push(c);
    }
    p = collapsed;
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p
    }
}

/// Per-path freshness index derived from a message list (see module docs).
#[derive(Debug, Default)]
pub struct FileStateIndex {
    /// Index of the newest `read_file` result per normalized path.
    pub last_read: HashMap<String, usize>,
    /// Index of the newest *successful* file mutation per normalized path.
    pub last_mutate: HashMap<String, usize>,
}

/// Map a successful file-tool call to the paths it mutated (v1 table):
///
/// - `write_file` / `append_file` / `apply_diff` / `delete` → `path`
/// - `copy` → `dest` only (copy does not mutate `src`)
/// - `move` → `src` (gone) and `dest` (new content)
///
/// Everything else (`shell`, search tools, …) mutates nothing trackable in v1.
fn mutated_paths(name: &str, params: &ToolParams) -> Vec<String> {
    match name {
        "write_file" | "append_file" | "apply_diff" | "delete" => {
            params.get::<String>("path").into_iter().collect()
        }
        "copy" => params.get::<String>("dest").into_iter().collect(),
        "move" => params
            .get::<String>("src")
            .into_iter()
            .chain(params.get::<String>("dest"))
            .collect(),
        _ => Vec::new(),
    }
}

/// `tool_call_id → (tool name, raw argument JSON)`, from assistant messages.
///
/// `role: "tool"` results carry only the id; the tool name + argument JSON
/// live on the paired assistant message. Built owned (not by reference) so the
/// caller may hold it across a mutable pass over the list.
pub fn call_map(messages: &[Message]) -> HashMap<String, (String, String)> {
    let mut calls = HashMap::new();
    for m in messages.iter().filter(|m| m.role == "assistant") {
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                calls.insert(
                    tc.id.clone(),
                    (tc.function.name.clone(), tc.function.arguments.clone()),
                );
            }
        }
    }
    calls
}

/// Build the per-path freshness index from a message list (see module docs).
///
/// One forward pass: indices only ever grow, so each map entry ends up holding
/// the *newest* occurrence per path.
pub fn build_file_state_index(messages: &[Message]) -> FileStateIndex {
    let mut index = FileStateIndex::default();
    let calls = call_map(messages);

    for (i, m) in messages.iter().enumerate() {
        if m.role != "tool" {
            continue;
        }
        let Some(call_id) = m.tool_call_id.as_deref() else {
            continue;
        };
        let Some((name, args)) = calls.get(call_id) else {
            continue;
        };
        let Ok(params) = parse_tool_args(args) else {
            continue;
        };

        if name == "read_file" {
            if let Some(path) = params.get::<String>("path") {
                index.last_read.insert(normalize_path(&path), i);
            }
            continue;
        }
        if m.content.starts_with(FAILURE_PREFIX) {
            continue; // failed call: nothing changed
        }
        for path in mutated_paths(name, &params) {
            index.last_mutate.insert(normalize_path(&path), i);
        }
    }

    index
}

/// True if `m` is a `read_file` tool result — a raw snapshot or an already-
/// collapsed freshness marker. `calls` must come from [`call_map`] over the
/// same message list.
pub fn is_read_file_result(calls: &HashMap<String, (String, String)>, m: &Message) -> bool {
    if m.role != "tool" {
        return false;
    }
    if is_file_marker(&m.content) {
        return true;
    }
    m.tool_call_id
        .as_deref()
        .and_then(|id| calls.get(id))
        .is_some_and(|(name, _)| name == "read_file")
}

/// Shrink stale/superseded pre-tail `read_file` results to one-line markers.
///
/// Legacy marker form of [`remove_stale_read_pairs`] (kept for compatibility
/// and tests): in-place content replacement only, `tool_call_id` pairing
/// stays intact and no tool message ever ends up empty. Messages at/after
/// `protect_from` (the protected tail) are never touched. Returns the number
/// of markers applied.
pub fn invalidate_stale_reads(
    messages: &mut [Message],
    protect_from: usize,
    index: &FileStateIndex,
) -> usize {
    let calls = call_map(messages);
    let mut applied = 0;

    for (i, m) in messages.iter_mut().enumerate() {
        if i >= protect_from {
            break;
        }
        if m.role != "tool" {
            continue;
        }
        if is_file_marker(&m.content) {
            continue; // already marked — idempotent
        }
        let Some(call_id) = m.tool_call_id.as_deref() else {
            continue;
        };
        let Some((name, args)) = calls.get(call_id) else {
            continue;
        };
        if name != "read_file" {
            continue;
        }
        let Ok(params) = parse_tool_args(args) else {
            continue;
        };
        let Some(path) = params.get::<String>("path") else {
            continue;
        };
        let key = normalize_path(&path);

        let is_stale = index.last_mutate.get(&key).is_some_and(|&mi| mi > i);
        let is_superseded = index.last_read.get(&key).is_some_and(|&ri| ri > i);

        let marker = if is_stale {
            stale_marker(&path)
        } else if is_superseded {
            superseded_marker(&path)
        } else {
            continue; // current: newest snapshot, nothing mutated since
        };
        m.content = marker;
        applied += 1;
    }

    applied
}

/// Remove the full tool pairs for every pre-tail `read_file` result that is
/// stale, superseded, or already collapsed to a freshness marker (a marker
/// left over from an older persisted session).
///
/// The span removed for a tool result at index `i` is its whole round: the
/// contiguous run of `role: "tool"` messages containing `i`, plus the
/// `assistant` message immediately before the run that issued the calls.
/// Removing the pair as a unit keeps call/result pairing intact (no orphaned
/// `tool_call_id`, no dangling call). Messages at/after `protect_from` (the
/// protected tail) are never touched. Returns the number of messages removed.
pub fn remove_stale_read_pairs(
    messages: &mut Vec<Message>,
    protect_from: usize,
    index: &FileStateIndex,
) -> usize {
    let calls = call_map(messages);
    let mut spans: Vec<(usize, usize)> = Vec::new();

    for i in 0..protect_from.min(messages.len()) {
        let m = &messages[i];
        if m.role != "tool" {
            continue;
        }
        let Some(call_id) = m.tool_call_id.as_deref() else {
            continue;
        };
        let Some((name, args)) = calls.get(call_id) else {
            continue;
        };
        if name != "read_file" {
            continue;
        }
        // Markers are dropped unconditionally; raw snapshots need the
        // freshness verdict.
        let drop = if is_file_marker(&m.content) {
            true
        } else {
            let Ok(params) = parse_tool_args(args) else {
                continue;
            };
            let Some(path) = params.get::<String>("path") else {
                continue;
            };
            let key = normalize_path(&path);
            let is_stale = index.last_mutate.get(&key).is_some_and(|&mi| mi > i);
            let is_superseded = index.last_read.get(&key).is_some_and(|&ri| ri > i);
            is_stale || is_superseded
        };
        if !drop {
            continue;
        }
        // Span = the full round: walk back over the contiguous tool run to
        // its start, then include the assistant message that issued it.
        let mut start = i;
        while start > 0 && messages[start - 1].role == "tool" {
            start -= 1;
        }
        if start > 0
            && messages[start - 1].role == "assistant"
            && messages[start - 1].tool_calls.is_some()
        {
            start -= 1;
        }
        let mut end = i + 1;
        while end < protect_from && messages[end].role == "tool" {
            end += 1;
        }
        spans.push((start, end));
    }

    if spans.is_empty() {
        return 0;
    }
    // Merge overlapping spans (two stale reads in the same round) and remove
    // from the end so earlier spans' indices stay valid.
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut removed = 0;
    for (s, e) in merged.iter().rev() {
        messages.drain(*s..*e);
        removed += e - s;
    }
    removed
}

#[cfg(test)]
mod tests;
