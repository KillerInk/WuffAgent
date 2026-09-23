//! Unit tests for the `filestate` module (see `super`).

use super::*;

fn msg(role: &str, content: &str) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn assistant_call(call_id: &str, tool_name: &str, args: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: Some(vec![crate::types::ToolCall {
            id: call_id.into(),
            call_type: "function".into(),
            function: crate::types::ToolFunction {
                name: tool_name.into(),
                arguments: args.into(),
            },
        }]),
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn tool_result(call_id: &str, content: &str) -> Message {
    Message {
        role: "tool".into(),
        content: content.into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: Some(call_id.into()),
        reasoning_content: None,
        image: None,
    }
}

#[test]
fn test_normalize_path_forms() {
    assert_eq!(normalize_path("./src/a.rs"), normalize_path("src/a.rs"));
    assert_eq!(normalize_path("src\\a\\b.rs"), "src/a/b.rs");
    assert_eq!(normalize_path("  ./src//a.rs  "), "src/a.rs");
    // Absolute paths keep their leading separator.
    assert_eq!(normalize_path("/usr//a/b.rs"), "/usr/a/b.rs");
}

#[test]
#[cfg(windows)]
fn test_normalize_path_case_fold_on_windows() {
    assert_eq!(normalize_path("SRC\\A.RS"), "src/a.rs");
}

#[test]
fn test_is_file_marker() {
    assert!(is_file_marker(
        "[read_file of a.rs — stale: file modified after this read; re-read before relying on it]"
    ));
    assert!(is_file_marker(
        "[read_file of a.rs — superseded by a newer read of the same file]"
    ));
    assert!(!is_file_marker("file contents here"));
    assert!(!is_file_marker(""));
}

#[test]
fn test_index_reads_and_mutations() {
    let messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"./src/a.rs\"}"),
        tool_result("c1", "content-a"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"src\\\\a.rs\",\"content\":\"x\"}",
        ),
        tool_result("c2", "ok"),
        assistant_call("c3", "read_file", "{\"path\":\"src/a.rs\"}"),
        tool_result("c3", "content-a2"),
    ];
    let index = build_file_state_index(&messages);
    // Path forms normalize to one key; newest indices win.
    assert_eq!(index.last_read.get("src/a.rs"), Some(&6));
    assert_eq!(index.last_mutate.get("src/a.rs"), Some(&4));
}

#[test]
fn test_index_failed_mutation_ignored() {
    let messages = vec![
        assistant_call("c1", "write_file", "{\"path\":\"a.rs\",\"content\":\"x\"}"),
        tool_result("c1", "Error: disk full"),
    ];
    let index = build_file_state_index(&messages);
    assert!(index.last_mutate.is_empty());
}

#[test]
fn test_index_copy_move_delete_semantics() {
    // copy: only dest mutates
    let copy_msgs = vec![
        assistant_call("c1", "copy", "{\"src\":\"a.rs\",\"dest\":\"b.rs\"}"),
        tool_result("c1", "ok"),
    ];
    let index = build_file_state_index(&copy_msgs);
    assert_eq!(index.last_mutate.get("b.rs"), Some(&1));
    assert!(!index.last_mutate.contains_key("a.rs"));

    // move: both src and dest mutate
    let move_msgs = vec![
        assistant_call("c1", "move", "{\"src\":\"a.rs\",\"dest\":\"b.rs\"}"),
        tool_result("c1", "ok"),
    ];
    let index = build_file_state_index(&move_msgs);
    assert_eq!(index.last_mutate.get("a.rs"), Some(&1));
    assert_eq!(index.last_mutate.get("b.rs"), Some(&1));

    // delete: path mutates
    let del_msgs = vec![
        assistant_call("c1", "delete", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "(no output)"),
    ];
    let index = build_file_state_index(&del_msgs);
    assert_eq!(index.last_mutate.get("a.rs"), Some(&1));
}

#[test]
fn test_index_shell_and_search_ignored() {
    let messages = vec![
        assistant_call("c1", "shell", "{\"command\":\"cargo build\"}"),
        tool_result("c1", "ok"),
        assistant_call(
            "c2",
            "search_content",
            "{\"pattern\":\"x\",\"path\":\"a.rs\"}",
        ),
        tool_result("c2", "hits"),
    ];
    let index = build_file_state_index(&messages);
    assert!(index.last_mutate.is_empty());
    assert!(index.last_read.is_empty());
}

#[test]
fn test_index_bad_args_skipped() {
    let messages = vec![
        assistant_call("c1", "read_file", "not-json"),
        tool_result("c1", "content"),
        assistant_call("c2", "write_file", "{}"),
        tool_result("c2", "ok"),
    ];
    let index = build_file_state_index(&messages);
    assert!(index.last_read.is_empty());
    assert!(index.last_mutate.is_empty());
}

#[test]
fn test_invalidate_stale_and_superseded() {
    // read A → write A → read A: first read stale, second read current.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old-content"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        assistant_call("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", "new-content"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let applied = invalidate_stale_reads(&mut messages, 7, &index);
    assert_eq!(applied, 1);
    assert!(messages[2].content.contains("stale: file modified"));
    assert_eq!(messages[6].content, "new-content");
    // Pairing intact.
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("c1"));
}

#[test]
fn test_invalidate_superseded_without_mutation() {
    // read A → read A: first read superseded, second current.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "first"),
        assistant_call("c2", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c2", "second"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let applied = invalidate_stale_reads(&mut messages, 5, &index);
    assert_eq!(applied, 1);
    assert!(messages[2].content.contains("superseded by a newer read"));
    assert_eq!(messages[4].content, "second");
}

#[test]
fn test_invalidate_respects_protect_from() {
    // A stale read at/after protect_from is untouched.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        assistant_call("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", "fresh"),
    ];
    let index = build_file_state_index(&messages);
    // Tail starts at the last assistant tool call (index 5).
    let applied = invalidate_stale_reads(&mut messages, 5, &index);
    assert_eq!(applied, 1);
    assert!(messages[2].content.contains("stale: file modified"));
    assert_eq!(
        messages[6].content, "fresh",
        "protected read must be untouched"
    );
}

#[test]
fn test_invalidate_failed_mutation_keeps_read() {
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "Error: disk full"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let applied = invalidate_stale_reads(&mut messages, 5, &index);
    assert_eq!(applied, 0, "failed write must not invalidate the read");
    assert_eq!(messages[2].content, "old");
}

#[test]
fn test_invalidate_path_form_mismatch_still_matches() {
    // Read uses ./src/a.rs, write uses src\a.rs — normalization must link them.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"./src/a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"src\\\\a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let applied = invalidate_stale_reads(&mut messages, 5, &index);
    assert_eq!(applied, 1);
    assert!(messages[2].content.contains("stale: file modified"));
}

#[test]
fn test_invalidate_idempotent() {
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    assert_eq!(invalidate_stale_reads(&mut messages, 5, &index), 1);
    let first = messages[2].content.clone();
    // Second pass: already-marked message is skipped.
    assert_eq!(invalidate_stale_reads(&mut messages, 5, &index), 0);
    assert_eq!(messages[2].content, first);
}

#[test]
fn test_invalidate_no_tool_messages_noop() {
    let mut messages = vec![msg("user", "q1"), msg("assistant", "a1"), msg("user", "q2")];
    let index = build_file_state_index(&messages);
    let applied = invalidate_stale_reads(&mut messages, 3, &index);
    assert_eq!(applied, 0);
}

// ── remove_stale_read_pairs (wholesale pair removal) ───────────────────────

/// Every assistant tool-call id must have all of its results present, and
/// every tool result's id must match a present call.
fn assert_pairs_intact(messages: &[Message]) {
    let call_ids: std::collections::HashSet<&str> = messages
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|tc| tc.id.as_str())
        .collect();
    for m in messages.iter().filter(|m| m.role == "tool") {
        let id = m.tool_call_id.as_deref().unwrap_or("");
        assert!(
            call_ids.contains(id),
            "tool result {id} present without its paired tool call"
        );
    }
    for m in messages
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
    {
        assert!(
            messages
                .iter()
                .any(|t| t.tool_call_id.as_deref() == Some(m.id.as_str())),
            "tool call {} present without its result",
            m.id
        );
    }
}

#[test]
fn test_remove_stale_read_pairs_wholesale() {
    // read A → write A → read A: the stale first read's WHOLE pair
    // (assistant + result) is removed; the write and the current read stay.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old-content"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        assistant_call("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", "new-content"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 7, &index);

    assert_eq!(
        removed, 2,
        "assistant + tool result of the stale read must go"
    );
    assert_eq!(messages.len(), 6);
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert!(!messages
        .iter()
        .any(|m| m.content.starts_with(MARKER_PREFIX)));
    assert_eq!(
        messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c3"))
            .map(|m| m.content.as_str()),
        Some("new-content"),
        "current read must stay in full"
    );
    assert_pairs_intact(&messages);
}

#[test]
fn test_remove_superseded_read_pairs() {
    // read A → read A: the older read's pair is removed.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "first"),
        assistant_call("c2", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c2", "second"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 5, &index);

    assert_eq!(removed, 2);
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert_eq!(
        messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c2"))
            .map(|m| m.content.as_str()),
        Some("second")
    );
    assert_pairs_intact(&messages);
}

#[test]
fn test_remove_current_reads_untouched() {
    // A read with no later mutation or newer read is current: not removed.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "current"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 3, &index);

    assert_eq!(removed, 0);
    assert_eq!(messages.len(), 4);
    assert_pairs_intact(&messages);
}

#[test]
fn test_remove_respects_protect_from() {
    // A stale read at/after protect_from is untouched.
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        assistant_call("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", "fresh"),
    ];
    let index = build_file_state_index(&messages);
    // Tail starts at the last assistant tool call (index 5).
    let removed = remove_stale_read_pairs(&mut messages, 5, &index);

    assert_eq!(removed, 2, "stale pre-tail pair must go");
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert_eq!(
        messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c3"))
            .map(|m| m.content.as_str()),
        Some("fresh"),
        "protected read must be untouched"
    );
    assert_pairs_intact(&messages);
}

#[test]
fn test_remove_drops_marker_pairs() {
    // A marker left over from an older (marker-style) trim is dropped too —
    // the stub does not stay in context forever.
    let marker =
        "[read_file of a.rs — stale: file modified after this read; re-read before relying on it]";
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", marker),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 3, &index);

    assert_eq!(removed, 2);
    assert_eq!(messages.len(), 2);
    assert_pairs_intact(&messages);
}

#[test]
fn test_remove_idempotent() {
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call(
            "c2",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c2", "ok"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    assert_eq!(remove_stale_read_pairs(&mut messages, 5, &index), 2);
    // Re-deriving on the current list: nothing left to remove.
    let index2 = build_file_state_index(&messages);
    assert_eq!(remove_stale_read_pairs(&mut messages, 5, &index2), 0);
    assert_eq!(messages.len(), 4);
}

/// Assistant message carrying several tool calls in one round.
fn assistant_calls(calls: &[(&str, &str, &str)]) -> Message {
    Message {
        role: "assistant".into(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: Some(
            calls
                .iter()
                .map(|(id, name, args)| crate::types::ToolCall {
                    id: (*id).into(),
                    call_type: "function".into(),
                    function: crate::types::ToolFunction {
                        name: (*name).into(),
                        arguments: (*args).into(),
                    },
                })
                .collect(),
        ),
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

#[test]
fn test_remove_same_round_two_stale_reads_one_span() {
    // One assistant round with two read_file calls, both stale: the round is
    // removed once (merged span), not twice.
    let mut messages = vec![
        msg("user", "q"),
        assistant_calls(&[
            ("c1", "read_file", "{\"path\":\"a.rs\"}"),
            ("c2", "read_file", "{\"path\":\"b.rs\"}"),
        ]),
        tool_result("c1", "old-a"),
        tool_result("c2", "old-b"),
        assistant_call(
            "c3",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c3", "ok"),
        assistant_call(
            "c4",
            "write_file",
            "{\"path\":\"b.rs\",\"content\":\"new\"}",
        ),
        tool_result("c4", "ok"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 7, &index);

    assert_eq!(
        removed, 3,
        "the shared round (assistant + both results) goes once, as a unit"
    );
    assert_eq!(messages.len(), 6);
    assert_pairs_intact(&messages);
}

// ── Protected reads (active/large files keep their current snapshot) ───────

#[test]
fn test_index_counts_reads_per_path() {
    let messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "one"),
        assistant_call("c2", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c2", "two"),
        assistant_call("c3", "read_file", "{\"path\":\"b.rs\"}"),
        tool_result("c3", "three"),
        assistant_call(
            "c4",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"x\"}",
        ),
        tool_result("c4", "ok"),
        assistant_call("c5", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c5", "four"),
    ];
    let index = build_file_state_index(&messages);
    assert_eq!(index.read_count.get("a.rs"), Some(&3));
    assert_eq!(index.read_count.get("b.rs"), Some(&1));
    // Indices point at tool results (even positions here are assistants).
    assert_eq!(index.last_read.get("a.rs"), Some(&10));
    assert_eq!(index.last_mutate.get("a.rs"), Some(&8));
}

#[test]
fn test_is_protected_read_criteria() {
    let small_once = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"small.rs\"}"),
        tool_result("c1", "tiny"),
    ];
    let index = build_file_state_index(&small_once);
    assert!(
        !is_protected_read(&index, "small.rs", "tiny"),
        "small file read once: not protected"
    );

    let big = "x".repeat(5000);
    let big_once = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"big.rs\"}"),
        tool_result("c1", &big),
    ];
    let index = build_file_state_index(&big_once);
    assert!(
        is_protected_read(&index, "big.rs", &big),
        "large snapshot: protected even after a single read"
    );

    let small_twice = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"work.rs\"}"),
        tool_result("c1", "v1"),
        assistant_call("c2", "read_file", "{\"path\":\"work.rs\"}"),
        tool_result("c2", "v2"),
    ];
    let index = build_file_state_index(&small_twice);
    assert!(
        is_protected_read(&index, "work.rs", "v2"),
        "file read twice (actively worked on): protected"
    );
}

#[test]
fn test_shared_round_kept_when_it_carries_protected_read() {
    // One round reads A and B; A is then mutated. B's large snapshot is the
    // model's working copy of that file: the round must STAY and only the
    // stale A result is collapsed to a marker in place.
    let big_b = "B".repeat(5000);
    let mut messages = vec![
        msg("user", "q"),
        assistant_calls(&[
            ("c1", "read_file", "{\"path\":\"a.rs\"}"),
            ("c2", "read_file", "{\"path\":\"b.rs\"}"),
        ]),
        tool_result("c1", "old-a"),
        tool_result("c2", &big_b),
        assistant_call(
            "c3",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c3", "ok"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 6, &index);

    assert_eq!(removed, 0, "the shared round must not be removed");
    let c1 = messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("c1"))
        .unwrap();
    assert_eq!(
        c1.content,
        stale_marker("a.rs"),
        "the stale read is marked in place"
    );
    let c2 = messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("c2"))
        .unwrap();
    assert_eq!(c2.content, big_b, "the protected read stays in full");
    assert_pairs_intact(&messages);

    // Idempotent: the next pass finds the marker, keeps the round again, and
    // changes nothing.
    let index2 = build_file_state_index(&messages);
    let before: Vec<(String, String)> = messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();
    assert_eq!(remove_stale_read_pairs(&mut messages, 6, &index2), 0);
    let after: Vec<(String, String)> = messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();
    assert_eq!(before, after);
}

#[test]
fn test_shared_round_removed_when_other_read_not_protected() {
    // Same shape as the protected case but B is small and read once: legacy
    // behavior — the round is removed wholesale.
    let mut messages = vec![
        msg("user", "q"),
        assistant_calls(&[
            ("c1", "read_file", "{\"path\":\"a.rs\"}"),
            ("c2", "read_file", "{\"path\":\"b.rs\"}"),
        ]),
        tool_result("c1", "old-a"),
        tool_result("c2", "small-b"),
        assistant_call(
            "c3",
            "write_file",
            "{\"path\":\"a.rs\",\"content\":\"new\"}",
        ),
        tool_result("c3", "ok"),
        msg("user", "q2"),
    ];
    let index = build_file_state_index(&messages);
    let removed = remove_stale_read_pairs(&mut messages, 6, &index);

    assert_eq!(removed, 3, "assistant + both results go as one span");
    assert!(
        !messages.iter().any(|m| m.tool_call_id.as_deref() == Some("c2")),
        "the non-protected read goes with the round"
    );
    assert_pairs_intact(&messages);
}
