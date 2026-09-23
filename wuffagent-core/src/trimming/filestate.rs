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
//!
//! Exception: a round that ALSO carries a current, protected read of another
//! file (read multiple times, or large — see [`is_protected_read`]) is kept;
//! the stale read in it is collapsed to a one-line marker in place instead,
//! so the other file's working snapshot is not sacrificed.

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
    /// How many times each normalized path was read in this conversation
    /// (failed reads count too — they occupy a round like any other read).
    /// Feeds [`is_protected_read`]; taken over the FULL list, so "read
    /// multiple times" means what it says even after the freshness pass has
    /// dropped old snapshots.
    pub read_count: HashMap<String, usize>,
}

/// A current snapshot is worth protecting from age-based removal (its pair is
/// deferred to the end of the removal order) when the file was read at least
/// this many times in the conversation — the model is actively working on it
/// and would otherwise edit from a stale memory of the trimmed snapshot.
const PROTECTED_READ_MIN_COUNT: usize = 2;

/// …or when the snapshot itself is at least this large — re-reading a big
/// file costs real tokens and a round, so its current snapshot stays in
/// context as long as the budget allows.
const PROTECTED_READ_MIN_CHARS: usize = 4096;

/// True if the current snapshot of `key` is worth protecting from age-based
/// removal: the file was read multiple times in this conversation (the model
/// is actively working on it) or the snapshot is large (re-reading it is
/// expensive). See the age pass of `ContextTrimming::trim_messages`.
pub fn is_protected_read(index: &FileStateIndex, key: &str, snapshot: &str) -> bool {
    index
        .read_count
        .get(key)
        .is_some_and(|&count| count >= PROTECTED_READ_MIN_COUNT)
        || snapshot.len() >= PROTECTED_READ_MIN_CHARS
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
                let key = normalize_path(&path);
                index.last_read.insert(key.clone(), i);
                *index.read_count.entry(key).or_insert(0) += 1;
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
/// `tool_call_id`, no dangling call).
///
/// Exception: if the round ALSO carries a protected current read of another
/// file (see [`is_protected_read`]), removing it wholesale would delete the
/// model's working snapshot of that file — so the stale read is collapsed to
/// a one-line marker IN PLACE (the legacy form) and the round is kept.
///
/// Messages at/after `protect_from` (the protected tail) are never touched.
/// Returns the number of messages removed (in-place marks are not counted).
pub fn remove_stale_read_pairs(
    messages: &mut Vec<Message>,
    protect_from: usize,
    index: &FileStateIndex,
) -> usize {
    let calls = call_map(messages);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    // (index, marker) for stale reads whose round must be kept: the round
    // also carries a protected current read of another file, so the stale
    // read is marked in place instead of dragging the round down.
    let mut marks: Vec<(usize, String)> = Vec::new();

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
        // Markers are dropped unconditionally and need no rewriting; raw
        // snapshots need the freshness verdict and remember which marker a
        // kept round would get.
        let kept_marker: Option<String> = if is_file_marker(&m.content) {
            None
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
            if is_stale {
                Some(stale_marker(&path))
            } else if is_superseded {
                Some(superseded_marker(&path))
            } else {
                continue; // current: newest snapshot, nothing mutated since
            }
        };
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
        // A round that also carries a protected current read of another file
        // is kept: dropping it wholesale would delete that snapshot and the
        // model would edit from a stale memory of the file.
        if round_holds_protected_read(messages, &calls, index, start, end, i) {
            if let Some(marker) = kept_marker {
                marks.push((i, marker));
            }
            continue;
        }
        spans.push((start, end));
    }

    if !marks.is_empty() {
        tracing::info!(
            "trimming: marked {} stale read(s) in place: their round(s) carry a protected current read of another file",
            marks.len()
        );
        for (i, marker) in marks {
            messages[i].content = marker;
        }
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

/// True if the round span `start..end` contains a `read_file` result (other
/// than `except`) whose snapshot is CURRENT — the newest read of its path
/// with no successful mutation since — and worth protecting (see
/// [`is_protected_read`]).
fn round_holds_protected_read(
    messages: &[Message],
    calls: &HashMap<String, (String, String)>,
    index: &FileStateIndex,
    start: usize,
    end: usize,
    except: usize,
) -> bool {
    for (offset, m) in messages[start..end].iter().enumerate() {
        let j = start + offset;
        if j == except || m.role != "tool" {
            continue;
        }
        if is_file_marker(&m.content) {
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
        let Ok(params) = parse_tool_args(args) else {
            continue;
        };
        let Some(path) = params.get::<String>("path") else {
            continue;
        };
        let key = normalize_path(&path);
        let Some(&latest) = index.last_read.get(&key) else {
            continue;
        };
        if latest != j {
            continue; // not the newest read of this path
        }
        if index.last_mutate.get(&key).is_some_and(|&mi| mi > j) {
            continue; // stale: not a working snapshot
        }
        if is_protected_read(index, &key, &m.content) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
