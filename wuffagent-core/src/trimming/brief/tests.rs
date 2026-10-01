//! Unit tests for the `brief` module (see `super`).

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
        notes: vec!["pinned: the event channel design is settled".into()],
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

#[test]
fn apply_note_anchors_after_system_before_task() {
    let mut messages = vec![
        {
            let mut m = user("sys");
            m.role = "system".into();
            m
        },
        user("task"),
        assistant("thinking"),
    ];
    let idx = apply_note(&mut messages, "decision: keep the marker stable")
        .expect("note applied");
    assert_eq!(messages.len(), 4);
    assert_eq!(idx, 1, "right after the system prompt, before the task");
    assert!(is_note_message(&messages[1]));
    assert_eq!(
        note_content(&messages[1]),
        Some("decision: keep the marker stable")
    );
    assert_eq!(messages[2].content, "task");
}

#[test]
fn apply_note_dedupes_identical_notes() {
    let mut messages = vec![user("task")];
    apply_note(&mut messages, "one").unwrap();
    let len = messages.len();
    // Identical text: no second message, returns the existing index.
    let idx = apply_note(&mut messages, "one").unwrap();
    assert_eq!(messages.len(), len);
    assert!(is_note_message(&messages[idx]));
    assert_eq!(
        messages.iter().filter(|m| is_note_message(m)).count(),
        1
    );
}

#[test]
fn apply_note_folds_oldest_into_brief_at_cap() {
    let mut messages = vec![user("task")];
    // A brief with existing state, so the folded note has a home.
    let brief = SessionBrief {
        task: "task".into(),
        ..Default::default()
    };
    apply_brief(&mut messages, &render(&brief));
    for i in 0..NOTES_MAX_ITEMS {
        apply_note(&mut messages, &format!("note {i}")).unwrap();
    }
    assert_eq!(
        messages.iter().filter(|m| is_note_message(m)).count(),
        NOTES_MAX_ITEMS
    );
    // One more: the oldest note ("note 0") is folded into the brief's
    // Notes: section and its message removed; the new one is anchored.
    apply_note(&mut messages, "note 3").unwrap();
    let notes: Vec<&Message> = messages.iter().filter(|m| is_note_message(m)).collect();
    assert_eq!(notes.len(), NOTES_MAX_ITEMS);
    assert!(!notes.iter().any(|m| note_content(m) == Some("note 0")));
    let brief_msg = messages.iter().find(|m| is_brief_message(m)).unwrap();
    let parsed = from_rendered(&brief_msg.content).unwrap();
    assert!(
        parsed.notes.iter().any(|n| n.starts_with("note 0")),
        "folded note lives in the brief's Notes: section, got {:?}",
        parsed.notes
    );
}

#[test]
fn reanchor_notes_pulls_drifted_notes_back() {
    // Drift: notes landed at the END of the list (the post-reload shape:
    // store append order) — reanchoring must move them right after the
    // system prompt, in list order.
    let mut messages = vec![
        {
            let mut m = user("sys");
            m.role = "system".into();
            m
        },
        user("task"),
        assistant("work"),
        user(&format!("{NOTE_MARKER}old note]")),
        user(&format!("{NOTE_MARKER}new note]")),
    ];
    reanchor_notes(&mut messages);
    assert_eq!(messages.len(), 5, "reanchoring moves, never adds");
    assert!(is_note_message(&messages[1]), "first note at slot 1");
    assert!(is_note_message(&messages[2]), "second note at slot 2");
    assert_eq!(note_content(&messages[1]), Some("old note"));
    assert_eq!(note_content(&messages[2]), Some("new note"));
    assert_eq!(messages[3].content, "task");
    // Idempotent: already anchored is a no-op.
    let before: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    reanchor_notes(&mut messages);
    let after: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    assert_eq!(before, after);
}

#[test]
fn note_content_strips_only_the_closing_bracket() {
    // The note's own text may contain (or even end with) brackets: only
    // the LAST char — the closing bracket `apply_note` appended — is
    // removed.
    let m = user(&format!("{NOTE_MARKER}use [a] format]"));
    assert_eq!(note_content(&m), Some("use [a] format"));
    let m = user(&format!("{NOTE_MARKER}ends with bracket]"));
    assert_eq!(note_content(&m), Some("ends with bracket"));
}

#[test]
fn reanchor_notes_without_system_prompt() {
    let mut messages = vec![
        user("task"),
        assistant("work"),
        user(&format!("{NOTE_MARKER}pinned]")),
    ];
    reanchor_notes(&mut messages);
    assert!(is_note_message(&messages[0]), "note anchored at the front");
    assert_eq!(messages[1].content, "task");
}

#[test]
fn note_message_never_in_first_user_task_scan() {
    // The task is the first user message that is neither brief nor note:
    // a drifted note before the task must not count as the task.
    let messages = vec![
        user(&format!("{NOTE_MARKER}pinned]")),
        user("the real task"),
    ];
    let task_idx = messages
        .iter()
        .position(|m| {
            m.role == "user"
                && !is_brief_message(m)
                && !is_note_message(m)
        })
        .unwrap();
    assert_eq!(task_idx, 1);
    assert_eq!(messages[task_idx].content, "the real task");
}

// ── S4b: LLM brief polish (request build + response parse) ──────────

#[test]
fn polish_request_carries_only_brief_and_span() {
    let prev = format!("{BRIEF_MARKER} ... Task: do the thing\nDecisions:\n- use X");
    let dropped = vec![
        user("correction: use Y"),
        assistant_call("w1", "write_file", "{}"),
        assistant("I updated the file."),
    ];
    let req = polish_request(Some(&prev), &dropped);
    assert_eq!(req.len(), 2);
    assert_eq!(req[0].role, "system");
    assert!(req[0].content.contains("ONLY facts"), "no-hallucination rule present");
    assert_eq!(req[1].role, "user");
    assert!(req[1].content.contains(&prev), "old brief carried");
    assert!(req[1].content.contains("correction: use Y"), "dropped user line carried");
    assert!(req[1].content.contains("write_file"), "dropped tool call carried");
    assert!(
        req[1].content.contains("I updated the file."),
        "dropped assistant text carried"
    );
}

#[test]
fn polish_span_text_caps_and_keeps_newest() {
    let dropped: Vec<Message> = (0..40)
        .map(|i| user(&format!("message {i}: {}", "x".repeat(300))))
        .collect();
    let text = polish_span_text(&dropped);
    assert!(
        text.chars().count() <= POLISH_SPAN_MAX_CHARS + 64,
        "span capped"
    );
    assert!(text.contains("message 39"), "newest kept");
    assert!(text.contains("omitted"), "oldest marked omitted");
    assert!(!text.contains("message 0:"), "oldest dropped");
}

#[test]
fn polish_span_text_excludes_previous_brief() {
    // A previous brief render inside the dropped span must not be
    // flattened into the span — it is carried as the OLD BRIEF instead.
    let old = format!("{BRIEF_MARKER} ... Task: t");
    let dropped = vec![user(&old), user("new user line")];
    let req = polish_request(None, &dropped);
    assert!(req[1].content.contains("new user line"));
    assert!(!req[1].content.contains("Task: t"));
}

#[test]
fn parse_polish_parses_section_body_without_marker() {
    let out = "Here is the updated brief:\n\
               Task: refactor the trimming module\n\
               Done:\n- wrote the extraction heuristics\n\
               In progress: wire the brief into trim_messages\n\
               Decisions:\n- keep the deterministic fallback\n\
               Files touched:\n- a/brief.rs (write_file)";
    let b = parse_polish(out).expect("parsed");
    assert_eq!(b.task, "refactor the trimming module");
    assert_eq!(b.in_progress.as_deref(), Some("wire the brief into trim_messages"));
    assert_eq!(b.decisions, vec!["keep the deterministic fallback"]);
    assert_eq!(b.files_touched, vec!["a/brief.rs (write_file)"]);
}

#[test]
fn parse_polish_rejects_garbage() {
    assert!(parse_polish("").is_none());
    assert!(parse_polish("no sections here at all").is_none());
}

// ── S5: pre-trim session-note refresh ──

#[test]
fn note_refresh_line_prefers_newest_in_progress() {
    let dropped = vec![
        assistant("Currently running the full test suite"),
        assistant("Done with the wiring, will start the UI next"),
        user("keep going"),
        assistant("Next: fix the remaining two edge cases in brief.rs"),
    ];
    assert_eq!(
        note_refresh_line(&dropped).as_deref(),
        Some("Next: fix the remaining two edge cases in brief.rs")
    );
}

#[test]
fn note_refresh_line_falls_back_to_last_text_line() {
    let dropped = vec![
        assistant("Wrote the extraction heuristics and committed them"),
        user("ok"),
        assistant("I ran the build"),
    ];
    assert_eq!(
        note_refresh_line(&dropped).as_deref(),
        Some("I ran the build")
    );
}

#[test]
fn note_refresh_line_none_for_tool_only_span() {
    let dropped = vec![user("please read the plan"), user("42 lines total")];
    assert_eq!(note_refresh_line(&dropped), None);
}

#[test]
fn note_refresh_line_skips_brief_note_and_trivial_lines() {
    let brief = format!("{BRIEF_MARKER} ... Task: t");
    let note = format!("{NOTE_MARKER} S4b done]");
    let dropped = vec![
        user(&brief),
        user(&note),
        assistant("ok"), // 2 chars: below the 12-char threshold
        assistant("Done."),
        assistant("[TRIM] something"), // bracket marker line
    ];
    assert_eq!(note_refresh_line(&dropped), None);
}

#[test]
fn refreshed_note_text_no_existing_uses_state_line() {
    assert_eq!(
        refreshed_note_text(None, "Next: run the test suite"),
        "Next: run the test suite"
    );
}

#[test]
fn refreshed_note_text_appends_state_via_separator() {
    assert_eq!(
        refreshed_note_text(Some("S4b done, HEAD at 1537247"), "Next: implement S5"),
        "S4b done, HEAD at 1537247 || Next: implement S5"
    );
}

#[test]
fn refreshed_note_text_caps_by_cutting_old_part() {
    let old = "x".repeat(NOTE_INPUT_MAX);
    let out = refreshed_note_text(Some(&old), "Next: state");
    assert_eq!(out.chars().count(), NOTE_INPUT_MAX);
    assert!(out.ends_with(" || Next: state"), "fresh state survives whole");
    assert!(out.contains("… || "), "the cut old part is marked before the separator");
}
