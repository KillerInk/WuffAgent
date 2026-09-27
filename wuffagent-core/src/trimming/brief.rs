//! The session "mission brief" — a compact, capped, ROLLING summary of the
//! conversation parts that trimming dropped, re-inserted after every trim so
//! the model keeps the TASK, the user's CORRECTIONS, the DECISIONS, and the
//! CURRENT STATE — and knows that compaction happened (prevents context rot;
//! see autoplans/context-rot-prevention.md, step S1).
//!
//! Design constraints:
//! - **Deterministic** (no LLM, no I/O): the trim path must stay synchronous
//!   and cheap. Extraction is conservative line/keyword heuristics over the
//!   DROPPED span; a line lands in at most one section.
//! - **Stateless**: `ContextTrimming` stays `&self`. The previous brief is
//!   the rendered message already sitting in the list (persisted into the
//!   session file by store reconciliation, re-parsed via `from_rendered` —
//!   which also survives a session reload).
//! - **Anchored**: the rendered brief is a single user-role message carrying
//!   [`BRIEF_MARKER`]. `age_sweep` and `truncate_largest_message` skip it, so
//!   it survives later trims; each new trim re-renders it in place (never
//!   stacks).
//! - **Capped**: every field has a char cap, every list an item cap, and the
//!   render a total cap — the task is never evicted.

use crate::types::Message;

/// Marker prefix of the rendered brief message. STABLE: the anchor guards in
/// `age_sweep` / `truncate_largest_message` and `is_brief_message` rely on it.
pub const BRIEF_MARKER: &str = "[SESSION BRIEF";

/// Minimum chars of dropped content for a brief to be worth (re)building —
/// below this the brief itself would cost a comparable share of the savings.
pub const BRIEF_MIN_DROPPED_CHARS: usize = 1_000;

/// Hard cap on the rendered brief in chars (~1k tokens): negligible against a
/// 100k+ window, large enough to carry the session state.
pub const BRIEF_MAX_CHARS: usize = 3_500;

const TASK_MAX: usize = 800;
const CORRECTION_MAX: usize = 400;
const CORRECTIONS_MAX_ITEMS: usize = 6;
const DONE_MAX: usize = 120;
const DONE_MAX_ITEMS: usize = 12;
const IN_PROGRESS_MAX: usize = 300;
const DECISION_MAX: usize = 200;
const DECISIONS_MAX_ITEMS: usize = 8;
const FILE_MAX: usize = 80;
const FILES_MAX_ITEMS: usize = 12;

/// Tools whose effect outlives the trimmed round: the model should remember
/// which files it WROTE even after the write rounds are gone.
const FILE_MUTATING_TOOLS: &[&str] = &[
    "write_file", "apply_diff", "append_file", "mkdir", "move", "copy", "delete",
];

/// One line of the rendered brief that `from_rendered` must map back to a
/// section. The header strings are part of the render/parse contract.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Section {
    Done,
    InProgress,
    Decision,
    Correction,
    File,
}

/// The rolling session state. All fields are capped (see `merge_dropped` /
/// `enforce_total_cap`); `task` is never evicted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionBrief {
    /// The task: the first user message, verbatim (flattened to one line).
    pub task: String,
    /// Later user messages (follow-ups / corrections), oldest → newest.
    pub corrections: Vec<String>,
    /// "Done" lines from assistant text.
    pub completed: Vec<String>,
    /// Most recent "what's next" line (newest wins).
    pub in_progress: Option<String>,
    /// Decision lines from assistant text.
    pub decisions: Vec<String>,
    /// Files written/edited by dropped rounds, oldest → newest ("path (tool)").
    pub files_touched: Vec<String>,
}

impl SessionBrief {
    /// True when there is nothing to report (no render is inserted).
    pub fn is_empty(&self) -> bool {
        self.task.is_empty()
            && self.corrections.is_empty()
            && self.completed.is_empty()
            && self.in_progress.is_none()
            && self.decisions.is_empty()
            && self.files_touched.is_empty()
    }

    /// Fold the DROPPED span (chronological order) into `prev` (the previous
    /// brief, if any) with `task_seed` (the task still sitting in the list,
    /// if any) as the preferred task source.
    ///
    /// Task resolution order: `prev.task` (already extracted in an earlier
    /// trim) → `task_seed` (the verbatim first user message still in context)
    /// → first user message in the dropped span.
    ///
    /// Heuristics are intentionally conservative: an assistant line lands in
    /// at most ONE section (decisions > in-progress > done), short lines are
    /// ignored, and nothing is invented for content that does not match.
    pub fn merge_dropped(
        prev: Option<&SessionBrief>,
        task_seed: Option<&str>,
        dropped: &[Message],
    ) -> SessionBrief {
        let mut b: SessionBrief = prev.cloned().unwrap_or_default();
        for m in dropped {
            if is_brief_message(m) {
                continue; // our own previous render — already reflected in `prev`
            }
            match m.role.as_str() {
                "user" => {
                    if b.task.is_empty() {
                        let line = one_line(&m.content, TASK_MAX);
                        if !line.is_empty() {
                            b.task = line;
                        }
                        continue;
                    }
                    let line = one_line(&m.content, CORRECTION_MAX);
                    push_capped(&mut b.corrections, &line, CORRECTIONS_MAX_ITEMS);
                }
                "assistant" => {
                    for raw in m.content.lines() {
                        let line = raw.trim();
                        // Noise floor: very short lines and bracketed markers
                        // ("[TRIM] ...") are not state.
                        if line.len() < 12 || line.starts_with('[') {
                            continue;
                        }
                        let section = classify_line(line);
                        match section {
                            Some(Section::Decision) => {
                                push_capped(&mut b.decisions, &cap_chars(line, DECISION_MAX), DECISIONS_MAX_ITEMS)
                            }
                            Some(Section::InProgress) => {
                                b.in_progress = Some(cap_chars(line, IN_PROGRESS_MAX));
                            }
                            Some(Section::Done) => {
                                push_capped(&mut b.completed, &cap_chars(line, DONE_MAX), DONE_MAX_ITEMS)
                            }
                            _ => {}
                        }
                    }
                    if let Some(calls) = m.tool_calls.as_ref() {
                        for tc in calls {
                            if !FILE_MUTATING_TOOLS.contains(&tc.function.name.as_str()) {
                                continue;
                            }
                            if let Some(path) = path_from_args(&tc.function.arguments) {
                                let entry = format!("{} ({})", path, tc.function.name);
                                // Newest wins per path: drop the old entry for
                                // this path, then dedupe by full entry.
                                b.files_touched
                                    .retain(|f| !f.starts_with(&format!("{path} (")));
                                if !b.files_touched.contains(&entry) {
                                    push_capped(
                                        &mut b.files_touched,
                                        &cap_chars(&entry, FILE_MAX + 32),
                                        FILES_MAX_ITEMS,
                                    );
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Task seed: the verbatim task still in context is the best source
        // when no earlier brief captured it.
        if b.task.is_empty() {
            if let Some(seed) = task_seed {
                let line = one_line(seed, TASK_MAX);
                if !line.is_empty() {
                    b.task = line;
                }
            }
        }
        b
    }
}

/// True for the rendered mission-brief message (user role + marker prefix).
pub fn is_brief_message(m: &Message) -> bool {
    m.role == "user" && m.content.starts_with(BRIEF_MARKER)
}

/// Render the brief as its single anchored user message. Returns an empty
/// string when there is nothing to report (the caller inserts nothing).
///
/// The layout is a render/parse CONTRACT with [`SessionBrief::from_rendered`]
/// — keep the section headers in sync.
pub fn render(b: &SessionBrief) -> String {
    if b.is_empty() {
        return String::new();
    }
    let mut s = String::with_capacity(BRIEF_MAX_CHARS);
    s.push_str("[SESSION BRIEF — the earlier part of this conversation was compacted to fit the context window.\n");
    s.push_str("Current state (for details not listed here, re-read the file or re-run the command instead of assuming):\n");
    if !b.task.is_empty() {
        s.push_str(&format!("Task: {}\n", b.task));
    }
    if !b.completed.is_empty() {
        s.push_str("Done:\n");
        for d in &b.completed {
            s.push_str(&format!("- {d}\n"));
        }
    }
    if let Some(ip) = &b.in_progress {
        s.push_str(&format!("In progress: {ip}\n"));
    }
    if !b.decisions.is_empty() {
        s.push_str("Decisions:\n");
        for d in &b.decisions {
            s.push_str(&format!("- {d}\n"));
        }
    }
    if !b.corrections.is_empty() {
        s.push_str("User notes (read carefully, newest last):\n");
        for c in &b.corrections {
            s.push_str(&format!("- {c}\n"));
        }
    }
    if !b.files_touched.is_empty() {
        s.push_str("Files touched:\n");
        for f in &b.files_touched {
            s.push_str(&format!("- {f}\n"));
        }
    }
    s.pop(); // trailing newline of the last item
    s.push_str("\n]");
    s
}

/// Parse a previously rendered brief back into structured fields. `None` when
/// `text` is not a rendered brief (or does not contain any state).
pub fn from_rendered(text: &str) -> Option<SessionBrief> {
    if !text.starts_with(BRIEF_MARKER) {
        return None;
    }
    let mut b = SessionBrief::default();
    let mut section = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Task: ") {
            b.task = rest.trim().to_string();
            section = None;
        } else if line == "Done:" {
            section = Some(Section::Done);
        } else if let Some(rest) = line.strip_prefix("In progress: ") {
            b.in_progress = Some(rest.trim().to_string());
            section = None;
        } else if line == "Decisions:" {
            section = Some(Section::Decision);
        } else if line == "User notes (read carefully, newest last):" {
            section = Some(Section::Correction);
        } else if line == "Files touched:" {
            section = Some(Section::File);
        } else if let Some(item) = line.strip_prefix("- ") {
            let Some(sec) = section else { continue };
            let item = item.trim().to_string();
            match sec {
                Section::Done => b.completed.push(item),
                Section::Decision => b.decisions.push(item),
                Section::Correction => b.corrections.push(item),
                Section::File => b.files_touched.push(item),
                Section::InProgress => {}
            }
        }
    }
    if b.is_empty() {
        None
    } else {
        Some(b)
    }
}

/// Replace any existing brief message with `text` (or insert one) at the
/// anchor slot — right after the leading system prompt (or at the front when
/// there is none), and, when the verbatim task (the first user message) sits
/// there, right AFTER it: the task is the beginning of the conversation and
/// the brief summarizes what happened since. Empty `text` is a no-op.
pub fn apply_brief(messages: &mut Vec<Message>, text: &str) {
    if text.is_empty() {
        return;
    }
    let mut at = if messages.first().is_some_and(|m| m.role == "system") {
        1
    } else {
        0
    };
    // Keep the verbatim task before the brief.
    if at < messages.len() && messages[at].role == "user" {
        at += 1;
    }
    match messages.iter().position(is_brief_message) {
        Some(i) => {
            messages[i].content = text.to_string();
            if i != at {
                let msg = messages.remove(i);
                messages.insert(at, msg);
            }
        }
        None => {
            messages.insert(
                at,
                Message {
                    role: "user".into(),
                    content: text.to_string(),
                    timestamp: String::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    image: None,
                },
            );
        }
    }
}

/// Evict oldest list items (never the task) until the render fits
/// [`BRIEF_MAX_CHARS`].
pub fn enforce_total_cap(b: &mut SessionBrief) {
    while render(b).chars().count() > BRIEF_MAX_CHARS {
        if !drop_oldest(b) {
            break;
        }
    }
}

/// Drop the single oldest item from the longest non-task section.
fn drop_oldest(b: &mut SessionBrief) -> bool {
    let lens = [
        b.completed.len(),
        b.corrections.len(),
        b.decisions.len(),
        b.files_touched.len(),
    ];
    let Some(idx) = (0..4).max_by_key(|&i| lens[i]).filter(|&i| lens[i] > 0) else {
        return false;
    };
    match idx {
        0 => {
            b.completed.remove(0);
        }
        1 => {
            b.corrections.remove(0);
        }
        2 => {
            b.decisions.remove(0);
        }
        _ => {
            b.files_touched.remove(0);
        }
    }
    true
}

/// Classify one assistant text line. Order = priority: decisions and
/// "what's next" lines carry the most value; plain progress bullets last.
fn classify_line(line: &str) -> Option<Section> {
    let l = line.to_ascii_lowercase();
    if contains_any(&l, &["decided", "decision", "chose", "will use", "went with", "opted"]) {
        return Some(Section::Decision);
    }
    if contains_any(
        &l,
        &["in progress", "currently", "next, i", "next step", "next:", "todo:", "remaining step", "still to do", "then i will"],
    ) {
        return Some(Section::InProgress);
    }
    if line.starts_with("- ")
        || line.starts_with("• ")
        || line.starts_with("* ")
        || line.starts_with("✓ ")
        || contains_any(
            &l,
            &["done", "completed", "finished", "created", "added", "removed", "fixed", "updated", "committed", "rebuilt"],
        )
    {
        return Some(Section::Done);
    }
    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Append `item` (deduped) to `list`, evicting the OLDEST item past the cap.
fn push_capped(list: &mut Vec<String>, item: &str, max_items: usize) {
    if item.trim().is_empty() {
        return;
    }
    if list.contains(&item.to_string()) {
        return;
    }
    list.push(item.to_string());
    while list.len() > max_items {
        list.remove(0);
    }
}

/// Truncate to `cap` chars (char boundary), marking the cut with "…".
fn cap_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Flatten whitespace (incl. newlines) to single spaces, then cap.
fn one_line(s: &str, cap: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    cap_chars(&flat, cap)
}

/// First of `path` / `dest` / `src` in a tool-call's JSON arguments.
fn path_from_args(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    for key in ["path", "dest", "src"] {
        if let Some(p) = v.get(key).and_then(|x| x.as_str()) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message {
            role: "user".into(),
            content: text.into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }
    }

    fn assistant(text: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: text.into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }
    }

    fn assistant_call(call_id: &str, tool: &str, args: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: call_id.into(),
                call_type: "function".into(),
                function: crate::types::ToolFunction {
                    name: tool.into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }
    }

    #[test]
    fn extract_task_corrections_done_decisions_files() {
        let dropped = vec![
            user("Refactor the trimming module and keep the tests green"),
            assistant(
                "I will use the new module split.\n\
                 - created brief.rs with the extraction heuristics\n\
                 - updated the age sweep guards\n\
                 Next step: wire the brief into trim_messages.",
            ),
            assistant_call("w1", "write_file", "{\"path\":\"M:/x/brief.rs\"}"),
            assistant_call("w2", "apply_diff", "{\"path\":\"M:/x/trim.rs\"}"),
            user("no, do not touch the client code"),
        ];
        let b = SessionBrief::merge_dropped(None, None, &dropped);
        assert_eq!(b.task, "Refactor the trimming module and keep the tests green");
        assert_eq!(b.corrections, vec!["no, do not touch the client code"]);
        assert_eq!(b.decisions, vec!["I will use the new module split."]);
        assert_eq!(b.in_progress, Some("Next step: wire the brief into trim_messages.".into()));
        assert_eq!(b.completed.len(), 2);
        assert!(b.completed[0].contains("created brief.rs"));
        assert!(b.files_touched.iter().any(|f| f.starts_with("M:/x/brief.rs (write_file)")));
        assert!(b.files_touched.iter().any(|f| f.starts_with("M:/x/trim.rs (apply_diff)")));
    }

    #[test]
    fn merge_keeps_prev_and_evicts_oldest_corrections() {
        let prev = SessionBrief {
            task: "do the thing".into(),
            corrections: (0..6).map(|i| format!("correction {i}")).collect(),
            ..Default::default()
        };
        let dropped = vec![user("correction 6"), user("correction 7")];
        let b = SessionBrief::merge_dropped(Some(&prev), None, &dropped);
        assert_eq!(b.task, "do the thing");
        assert_eq!(b.corrections.len(), CORRECTIONS_MAX_ITEMS);
        assert_eq!(b.corrections[0], "correction 2", "oldest corrections evicted");
        assert_eq!(*b.corrections.last().unwrap(), "correction 7");
        // prev must not be mutated
        assert_eq!(prev.corrections.len(), 6);
        // task from prev wins over the seed
        let b2 = SessionBrief::merge_dropped(Some(&prev), Some("seed task"), &dropped);
        assert_eq!(b2.task, "do the thing");
    }

    #[test]
    fn task_seed_used_when_no_prev() {
        let b = SessionBrief::merge_dropped(None, Some("  build   the \nagent panel"), &[]);
        assert_eq!(b.task, "build the agent panel");
        assert!(b.is_empty() || !b.task.is_empty());
    }

    #[test]
    fn caps_and_one_line_flattening() {
        let long_task = "x".repeat(2_000);
        let b = SessionBrief::merge_dropped(None, None, &vec![user(&format!("{long_task}\nsecond line"))]);
        assert!(b.task.chars().count() <= TASK_MAX, "task capped");
        assert!(!b.task.contains('\n'), "task flattened to one line");
        assert!(b.task.ends_with('…'), "truncation marked");
    }

    #[test]
    fn total_cap_evicts_but_keeps_task() {
        let mut b = SessionBrief {
            task: "keep me".into(),
            completed: (0..40)
                .map(|i| format!("done item {i:02} {}", "p".repeat(110)))
                .collect(),
            ..Default::default()
        };
        enforce_total_cap(&mut b);
        assert!(render(&b).chars().count() <= BRIEF_MAX_CHARS, "total cap holds");
        assert_eq!(b.task, "keep me", "task is never evicted");
    }

    #[test]
    fn render_parse_roundtrip() {
        let b = SessionBrief {
            task: "Ship the improvements panel".into(),
            corrections: vec!["skip the settings dialog".into()],
            completed: vec!["- created draw.rs".into(), "fixed the id collision".into()],
            in_progress: Some("Next: wire the event channel".into()),
            decisions: vec!["will use the AppEvent channel for the done signal".into()],
            files_touched: vec!["M:/wuffagent-egui/src/ui/improvements/draw.rs (write_file)".into()],
        };
        let text = render(&b);
        assert!(text.starts_with(BRIEF_MARKER));
        assert!(text.ends_with("]"));
        let parsed = from_rendered(&text).expect("own render must parse");
        assert_eq!(parsed, b, "render → parse must round-trip");
    }

    #[test]
    fn from_rendered_rejects_non_brief() {
        assert!(from_rendered("hello").is_none());
        assert!(from_rendered(&render(&SessionBrief::default())).is_none());
    }

    #[test]
    fn is_brief_message_checks_role_and_marker() {
        let mut m = user(BRIEF_MARKER);
        assert!(is_brief_message(&m));
        m.role = "assistant".into();
        assert!(!is_brief_message(&m));
        m = user("[SESSION BRIEFS"); // marker prefix without the space boundary
        assert!(is_brief_message(&m), "prefix match is the contract");
    }

    #[test]
    fn merge_skips_previous_brief_message() {
        let prev = SessionBrief {
            task: "old task".into(),
            ..Default::default()
        };
        let old_render = render(&prev);
        // The old render must not leak its lines (e.g. "Task: old task") in.
        let dropped = vec![user(&old_render), user("new correction")];
        let b = SessionBrief::merge_dropped(Some(&prev), None, &dropped);
        assert_eq!(b.task, "old task");
        assert_eq!(b.corrections, vec!["new correction"]);
    }

    #[test]
    fn files_newest_wins_per_path() {
        let dropped = vec![
            assistant_call("a", "write_file", "{\"path\":\"M:/f.rs\"}"),
            assistant_call("b", "apply_diff", "{\"path\":\"M:/f.rs\"}"),
        ];
        let b = SessionBrief::merge_dropped(None, None, &dropped);
        assert_eq!(b.files_touched, vec!["M:/f.rs (apply_diff)"]);
    }

    #[test]
    fn apply_brief_anchors_after_task_and_replaces_in_place() {
        let v1 = format!("{BRIEF_MARKER} v1");
        let v2 = format!("{BRIEF_MARKER} v2");
        let mut messages = vec![
            {
                let mut m = user("sys");
                m.role = "system".into();
                m
            },
            user("task"),
        ];
        apply_brief(&mut messages, &v1);
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[1].content, "task",
            "the verbatim task stays before the brief"
        );
        assert_eq!(messages[2].content, v1);
        // Re-apply: replaced in place, not stacked.
        apply_brief(&mut messages, &v2);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].content, "task");
        assert_eq!(messages[2].content, v2);
        assert_eq!(messages.iter().filter(|m| is_brief_message(m)).count(), 1);
        // No system prompt: anchored after the task (still index 1).
        let mut bare = vec![user("task")];
        apply_brief(&mut bare, &v1);
        assert_eq!(bare.len(), 2);
        assert_eq!(bare[0].content, "task");
        assert_eq!(bare[1].content, v1);
        // Empty text: no-op.
        let n = messages.len();
        apply_brief(&mut messages, "");
        assert_eq!(messages.len(), n);
    }
}
