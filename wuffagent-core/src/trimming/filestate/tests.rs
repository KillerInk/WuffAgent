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
    assert!(is_file_marker("[read_file of a.rs — stale: file modified after this read; re-read before relying on it]"));
    assert!(is_file_marker("[read_file of a.rs — superseded by a newer read of the same file]"));
    assert!(!is_file_marker("file contents here"));
    assert!(!is_file_marker(""));
}

#[test]
fn test_index_reads_and_mutations() {
    let messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"./src/a.rs\"}"),
        tool_result("c1", "content-a"),
        assistant_call("c2", "write_file", "{\"path\":\"src\\\\a.rs\",\"content\":\"x\"}"),
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
        assistant_call("c2", "search_content", "{\"pattern\":\"x\",\"path\":\"a.rs\"}"),
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
        assistant_call("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"new\"}"),
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
        assistant_call("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"new\"}"),
        tool_result("c2", "ok"),
        assistant_call("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", "fresh"),
    ];
    let index = build_file_state_index(&messages);
    // Tail starts at the last assistant tool call (index 5).
    let applied = invalidate_stale_reads(&mut messages, 5, &index);
    assert_eq!(applied, 1);
    assert!(messages[2].content.contains("stale: file modified"));
    assert_eq!(messages[6].content, "fresh", "protected read must be untouched");
}

#[test]
fn test_invalidate_failed_mutation_keeps_read() {
    let mut messages = vec![
        msg("user", "q"),
        assistant_call("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", "old"),
        assistant_call("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"new\"}"),
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
        assistant_call("c2", "write_file", "{\"path\":\"src\\\\a.rs\",\"content\":\"new\"}"),
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
        assistant_call("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"new\"}"),
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
