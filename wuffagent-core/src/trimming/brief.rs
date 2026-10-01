//! The session "mission brief" — a compact, capped, ROLLING summary of the
//! conversation parts that trimming dropped, re-inserted after every trim so
//! the model keeps the TASK, the user's CORRECTIONS, the DECISIONS, and the
//! CURRENT STATE — and knows that compaction happened (prevents context rot;
//! see autoplans/context-rot-prevention.md, steps S1/S4a) — plus the
//! "session note" mechanism (S4a): agent-authored state notes anchored after
//! the system prompt, protected from every trim, and folded into the brief's
//! `Notes:` section when the note cap is reached.
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
//!   [`BRIEF_MARKER`], and each session note is a user-role message carrying
//!   [`NOTE_MARKER`]. `age_sweep` and `truncate_largest_message` skip both,
//!   so they survive later trims; each new trim re-renders the brief in
//!   place (never stacks).
//! - **Capped**: every field has a char cap, every list an item cap, and the
//!   render a total cap — the task is never evicted.
//!
//! NOTE on "first user message = the task": with anchored notes in the list
//! the first user message is usually a NOTE, not the task. Every site that
//! resolves "the task" must look for the first user message that is neither
//! a brief nor a note (see the task guards in `summarizer/trim.rs` and the
//! `task_seed` in `summarizer/mod.rs`).

use crate::types::Message;

/// Marker prefix of the rendered brief message. STABLE: the anchor guards in
/// `age_sweep` / `truncate_largest_message` and `is_brief_message` rely on it.
pub const BRIEF_MARKER: &str = "[SESSION BRIEF";

/// Marker prefix of a session-note message (S4a). STABLE: `is_note_message`,
/// the `age_sweep` / `truncate_largest_message` guards and the task
/// disambiguation rely on it.
pub const NOTE_MARKER: &str = "[SESSION NOTE — ";

/// Cap for tool input to `session_note` (chars): longer notes are truncated
/// with an ellipsis marker — a note is a pointer, not a transcript.
pub const NOTE_INPUT_MAX: usize = 2_000;

/// Cap for a note line once folded into the brief's `Notes:` section.
const NOTE_MAX: usize = 400;

/// Max anchored note messages kept in the request list (S4a): the oldest is
/// folded into the brief (top priority) before a new one is inserted.
pub const NOTES_MAX_ITEMS: usize = 3;

/// Minimum chars of dropped content for a brief to be worth (re)building —
/// below this the brief itself would cost a comparable share of the savings.
pub const BRIEF_MIN_DROPPED_CHARS: usize = 1_000;

/// Hard cap on the rendered brief in chars (~1k tokens): negligible against a
/// 100k+ window, large enough to carry the session state.
pub const BRIEF_MAX_CHARS: usize = 3_500;

/// S4b: minimum dropped-span size (chars) for the LLM brief polish to fire —
/// below this the deterministic brief is good enough and the extra LLM
/// round-trip is not worth it.
pub const BRIEF_POLISH_MIN_DROPPED_CHARS: usize = 4_000;

/// S4b: cap for the flattened dropped span inside the polish request — the
/// request carries ONLY the old brief + the span, so it must stay small by
/// construction (it cannot overflow the window).
pub const POLISH_SPAN_MAX_CHARS: usize = 12_000;

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
    "write_file", "apply_diff", "replace_lines", "append_file", "mkdir", "move", "copy", "delete",
];

/// One line of the rendered brief that `from_rendered` must map back to a
/// section. The header strings are part of the render/parse contract.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Section {
    Done,
    InProgress,
    Note,
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
    /// Agent-authored session notes (S4a): folded in when an anchored note
    /// message is evicted by the note cap, plus any note found in a dropped
    /// span. Newest appended; evicted LAST by [`enforce_total_cap`] (agent
    /// state outranks everything the heuristics can re-derive).
    pub notes: Vec<String>,
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
            && self.notes.is_empty()
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
                    if is_note_message(m) {
                        // Agent-authored state note (S4a): top priority — fold
                        // into the notes section, never the task/corrections.
                        if let Some(text) = note_content(m) {
                            push_capped(&mut b.notes, &one_line(text, NOTE_MAX), NOTES_MAX_ITEMS * 2);
                        }
                        continue;
                    }
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

/// True for a session-note message (S4a: user role + [`NOTE_MARKER`]).
pub fn is_note_message(m: &Message) -> bool {
    m.role == "user" && m.content.starts_with(NOTE_MARKER)
}

/// The note text of a note message (content after the marker, without the
/// closing bracket), if any. `apply_note` renders notes as
/// `{NOTE_MARKER}{note}]`, so the closing `]` is stripped back off here —
/// otherwise the bracket leaks into the brief's `Notes:` section whenever a
/// note is folded in. (A note whose own text ends in `]` still round-trips:
/// only the LAST char, which is always the closing bracket we appended, is
/// removed.)
pub fn note_content(m: &Message) -> Option<&str> {
    if is_note_message(m) {
        let body = m.content.strip_prefix(NOTE_MARKER)?;
        Some(body.strip_suffix(']').unwrap_or(body))
    } else {
        None
    }
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
    if !b.notes.is_empty() {
        s.push_str("Notes (agent-recorded state, read carefully):\n");
        for n in &b.notes {
            s.push_str(&format!("- {n}\n"));
        }
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
    parse_sections(text)
}

/// The render/parse section grammar, shared by [`from_rendered`] (which
/// additionally requires the marker) and [`parse_polish`] (S4b: the LLM
/// polish response carries the section body, usually without the marker).
/// `None` when no state could be recovered.
fn parse_sections(text: &str) -> Option<SessionBrief> {
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
        } else if line == "Notes (agent-recorded state, read carefully):" {
            section = Some(Section::Note);
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
                Section::Note => b.notes.push(item),
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

/// S4b: parse an LLM brief-polish response into a `SessionBrief` — the same
/// section grammar as [`from_rendered`], tolerant of the model's wrapper
/// text (preamble, code fences: non-matching lines are ignored). `None` when
/// nothing could be recovered — the caller keeps the deterministic brief.
pub fn parse_polish(text: &str) -> Option<SessionBrief> {
    parse_sections(text)
}

/// S4b: build the LLM brief-polish request — `[system, user]` carrying ONLY
/// the old brief render (if any) and the flattened dropped span (capped at
/// [`POLISH_SPAN_MAX_CHARS`], newest kept). Small by construction: it cannot
/// overflow the window. The response must be the brief body in the section
/// grammar of [`parse_polish`].
pub fn polish_request(prev_brief: Option<&str>, dropped: &[Message]) -> Vec<Message> {
    let system = "You are the context compressor for an AI agent's session. Part of its \
conversation was just compacted (trimmed) to fit the context window. Merge the OLD BRIEF \
and the DROPPED SPAN below into ONE updated mission brief so the agent can continue \
seamlessly without the trimmed history.\n\
Rules:\n\
- Use ONLY facts present in the OLD BRIEF or the DROPPED SPAN — never invent \
results, file contents, line numbers, or decisions that are not there.\n\
- Keep this exact format, omitting empty sections:\n\
Task: <the overall task, one line>\n\
Done:\n- <what was completed and still matters to continue>\n\
In progress: <the single most recent in-progress state, one line>\n\
Notes (agent-recorded state, read carefully):\n- <agent notes, verbatim>\n\
Decisions:\n- <decisions that still constrain the work>\n\
User notes (read carefully, newest last):\n- <user corrections and constraints>\n\
Files touched:\n- <path (tool)>\n\
- Drop items that are fully done and no longer needed to continue; keep the \
most recent state and anything that constrains the next steps.\n\
- Output ONLY the brief in that format — no commentary, no markdown fences.";
    let mut user = String::with_capacity(POLISH_SPAN_MAX_CHARS + 512);
    user.push_str("OLD BRIEF:\n");
    user.push_str(prev_brief.unwrap_or("(none)"));
    user.push_str("\n\nDROPPED SPAN (oldest to newest):\n");
    user.push_str(&polish_span_text(dropped));
    vec![
        Message {
            role: "system".to_string(),
            content: system.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        },
        Message {
            role: "user".to_string(),
            content: user,
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        },
    ]
}

/// S4b: flatten the dropped span to request-size text — one line per message
/// (`{role}: content`, tool-call names appended for assistant messages).
/// When the [`POLISH_SPAN_MAX_CHARS`] cap is hit the OLDEST lines are dropped
/// first: the state closest to the continuation point is what the brief must
/// keep.
fn polish_span_text(dropped: &[Message]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(dropped.len());
    for m in dropped {
        if is_brief_message(m) {
            continue; // carried as the OLD BRIEF instead
        }
        let mut line = format!("{}: {}", m.role, one_line(&m.content, 400));
        if let Some(calls) = m.tool_calls.as_deref() {
            let names: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();
            if !names.is_empty() {
                line.push_str(&format!(" [calls: {}]", names.join(", ")));
            }
        }
        if !line.trim().is_empty() {
            lines.push(line);
        }
    }
    let mut total: usize = lines.iter().map(|l| l.chars().count() + 1).sum();
    let mut omitted = 0usize;
    while total > POLISH_SPAN_MAX_CHARS && lines.len() > 1 {
        total -= lines.remove(0).chars().count() + 1;
        omitted += 1;
    }
    let mut out = if omitted > 0 {
        format!("[... {omitted} older dropped message(s) omitted ...]\n")
    } else {
        String::new()
    };
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    if out.is_empty() {
        "(empty)".to_string()
    } else {
        out
    }
}

/// Replace any existing brief message with `text` (or insert one) at the
/// anchor slot — right after the leading system prompt (or at the front when
/// there is none), past the anchored note block (S4a), and, when the verbatim
/// task (the first user message that is neither a brief nor a note) sits
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
    // Keep the anchored notes and the verbatim task before the brief.
    while at < messages.len() && is_note_message(&messages[at]) {
        at += 1;
    }
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

/// Insert a session note (S4a) as an anchored user message right after the
/// system prompt (before the task), or at the front when there is no system
/// prompt; multiple notes form a contiguous block in insertion order.
///
/// Caps: at most [`NOTES_MAX_ITEMS`] anchored notes — adding one beyond the
/// cap first FOLDS the oldest note's content into the session brief's
/// `Notes:` section (top priority; the brief is (re)inserted in place even
/// though no trim is happening), then removes the note message. The note's
/// state is therefore carried by the anchored message or by the brief — never
/// both lost. Duplicate notes (identical text) are a no-op.
///
/// Returns the index of the (new or existing) note message so the caller can
/// `record_in_store` it (the note must survive session reloads). `None` for
/// empty notes.
pub fn apply_note(messages: &mut Vec<Message>, note: &str) -> Option<usize> {
    let note = note.trim();
    if note.is_empty() {
        return None;
    }
    let text = format!("{NOTE_MARKER}{note}]");
    // Dedupe: the identical note is already in the list (a drifted position is
    // fixed by the next `reanchor_notes` pass).
    if let Some(i) = messages.iter().position(|m| m.content == text) {
        return Some(i);
    }
    if messages.iter().filter(|m| is_note_message(m)).count() >= NOTES_MAX_ITEMS {
        // Note cap reached: fold the OLDEST note into the brief first.
        let i = messages.iter().position(is_note_message)?;
        let msg = messages[i].clone();
        if let Some(folded) = note_content(&msg).map(|t| one_line(t, NOTE_MAX)) {
            let mut brief = messages
                .iter()
                .find(|m| is_brief_message(m))
                .and_then(|m| from_rendered(&m.content))
                .unwrap_or_default();
            push_capped(&mut brief.notes, &folded, NOTES_MAX_ITEMS * 2);
            enforce_total_cap(&mut brief);
            let brief_text = render(&brief);
            if !brief_text.is_empty() {
                apply_brief(messages, &brief_text);
            }
        }
        messages.remove(i);
    }
    let at = anchor_note_slot(messages);
    messages.insert(
        at,
        Message {
            role: "user".into(),
            content: text,
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        },
    );
    Some(at)
}

/// The slot where the NEXT anchored note goes: right after the leading
/// system prompt, past any already-anchored note block.
fn anchor_note_slot(messages: &[Message]) -> usize {
    let mut at = if messages.first().is_some_and(|m| m.role == "system") {
        1
    } else {
        0
    };
    while at < messages.len() && is_note_message(&messages[at]) {
        at += 1;
    }
    at
}

/// Re-anchor drifted session notes (S4a) into the contiguous block right
/// after the system prompt, in list order (oldest → newest), so the notes sit
/// "right after the system prompt" in EVERY request — including right after a
/// session reload, where the store's append order has them at the end of the
/// list. No-op when every note is already anchored.
pub fn reanchor_notes(messages: &mut Vec<Message>) {
    let sys_end = if messages.first().is_some_and(|m| m.role == "system") {
        1
    } else {
        0
    };
    let anchored = (sys_end..messages.len())
        .take_while(|&i| is_note_message(&messages[i]))
        .count();
    let total = messages.iter().filter(|m| is_note_message(m)).count();
    if anchored == total {
        return; // already anchored, in order
    }
    // Collect in list order (remove from the back to keep indices valid).
    let mut notes = Vec::with_capacity(total);
    for i in (0..messages.len()).rev() {
        if is_note_message(&messages[i]) {
            notes.push(messages.remove(i));
        }
    }
    notes.reverse();
    for (k, msg) in notes.into_iter().enumerate() {
        messages.insert(sys_end + k, msg);
    }
}

/// S5 (context-rot-prevention.md): the "state as of this compaction" line
/// for the proactive-trim note refresh — the NEWEST assistant line the
/// brief classifies as in-progress (the model's own "what's next" wins),
/// falling back to the newest non-trivial assistant text line in the span.
/// `None` when the span carries no assistant text worth noting (tool-only
/// spans). Brief/note messages are skipped (protected, so they never reach
/// a dropped span — belt-and-braces), as are bracket-marker lines and
/// <12-char lines (the extraction threshold).
pub fn note_refresh_line(dropped: &[Message]) -> Option<String> {
    let mut in_progress: Option<String> = None;
    let mut last_line: Option<String> = None;
    for m in dropped {
        if m.role.as_str() != "assistant" || is_brief_message(m) || is_note_message(m) {
            continue;
        }
        for raw in m.content.lines() {
            let line = raw.trim();
            if line.len() < 12 || line.starts_with('[') {
                continue;
            }
            let flat = one_line(line, IN_PROGRESS_MAX);
            match classify_line(line) {
                Some(Section::InProgress) => in_progress = Some(flat),
                _ => last_line = Some(flat),
            }
        }
    }
    in_progress.or(last_line)
}

/// S5 (context-rot-prevention.md): the refreshed note text for the
/// proactive-trim note update — the existing note (if any) with the fresh
/// state line appended via " || ", capped at [`NOTE_INPUT_MAX`] by
/// truncating the OLD part (the fresh state is the point of the refresh and
/// must survive whole; a cut is marked with "…"). With no existing note the
/// state line IS the note.
pub fn refreshed_note_text(existing: Option<&str>, state_line: &str) -> String {
    let state = state_line.trim();
    match existing.map(str::trim).filter(|e| !e.is_empty()) {
        None => one_line(state, NOTE_INPUT_MAX),
        Some(e) => {
            let sep = " || ";
            // Room for the OLD part (the fresh state + separator must fit
            // whole). `cap_chars` appends one "…" when it cuts, so reserve
            // that slot and the total lands exactly on NOTE_INPUT_MAX.
            let room = NOTE_INPUT_MAX
                .saturating_sub(state.chars().count())
                .saturating_sub(sep.chars().count());
            // `cap_chars` keeps exactly `room` chars when it cuts (incl. the
            // "…"), so the total lands exactly on NOTE_INPUT_MAX.
            let old_part = cap_chars(e, room);
            format!("{old_part}{sep}{state}")
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

/// Drop the single oldest item from the longest non-task section. Notes
/// (S4a, agent-authored state) sit LAST in the priority: on equal lengths the
/// oldest heuristic section is evicted first.
fn drop_oldest(b: &mut SessionBrief) -> bool {
    let lens = [
        b.completed.len(),
        b.corrections.len(),
        b.decisions.len(),
        b.files_touched.len(),
        b.notes.len(),
    ];
    let max_len = lens.iter().copied().max().unwrap_or(0);
    if max_len == 0 {
        return false;
    }
    let idx = (0..lens.len()).find(|&i| lens[i] == max_len).unwrap();
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
        3 => {
            b.files_touched.remove(0);
        }
        _ => {
            b.notes.remove(0);
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
mod tests;
