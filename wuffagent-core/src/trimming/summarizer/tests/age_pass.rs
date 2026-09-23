//! Protected current reads survive the age-based removal (multi-sweep).

use super::*;

/// Assistant message carrying several tool calls in one round.
fn assistant_calls_multi(calls: &[(&str, &str, &str)]) -> Message {
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
fn test_age_pass_defers_removal_of_active_file_read() {
    // The model read a.rs twice (actively working on it); the newest read is
    // current (no later mutation). The over-budget history must be trimmed
    // from the OLD JUNK, not from the working snapshot: after the trim the
    // latest a.rs read must still be in context, in full.
    let snapshot = "A".repeat(3000); // small: protection comes from the count
    let mut messages = vec![user_msg("refactor a.rs")];
    for i in 0..15 {
        let cid = format!("junk_{i}");
        messages.push(assistant_tool_call(&cid, &format!("{{\"n\":{i}}}")));
        messages.push(tool_result(&cid, &"j".repeat(800)));
    }
    messages.push(assistant_call_named("r1", "read_file", "{\"path\":\"a.rs\"}"));
    messages.push(tool_result("r1", &snapshot));
    messages.push(assistant_call_named("r2", "read_file", "{\"path\":\"a.rs\"}"));
    messages.push(tool_result("r2", &snapshot));
    messages.push(user_msg("keep going"));

    let target = 8000usize;
    assert!(
        ContextTrimming::message_char_count(&messages) > target,
        "precondition: must start over budget"
    );

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, target, &make_config());

    assert!(
        ContextTrimming::message_char_count(&messages) <= target,
        "after trim must be under budget"
    );
    assert!(
        removed > 2,
        "junk (and the superseded first read) must be removed"
    );
    assert_pairs_intact(&messages);
    assert_eq!(
        tool_content_at(&messages, "r2"),
        snapshot,
        "the latest read of the actively-worked file must survive the trim in full"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("r1")),
        "the superseded first read still goes (freshness pass)"
    );
}

#[test]
fn test_age_pass_second_sweep_removes_protected_reads_last() {
    // Four files each read twice (all latest reads are protected by count);
    // only the protected reads and the tail exceed the budget. The first
    // sweep defers all of them; the second sweep must remove them (oldest
    // first) so the trim still converges.
    let mut messages = vec![user_msg("task")];
    for f in 0..4 {
        let cid1 = format!("r1_{f}");
        let cid2 = format!("r2_{f}");
        let args = format!("{{\"path\":\"f{f}.rs\"}}");
        messages.push(assistant_call_named(&cid1, "read_file", &args));
        messages.push(tool_result(&cid1, &format!("old-{f}")));
        messages.push(assistant_call_named(&cid2, "read_file", &args));
        messages.push(tool_result(&cid2, &"S".repeat(2500)));
    }
    messages.push(user_msg("go"));

    let target = 6000usize;
    assert!(
        ContextTrimming::message_char_count(&messages) > target,
        "precondition: must start over budget"
    );

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, target, &make_config());

    assert!(
        ContextTrimming::message_char_count(&messages) <= target,
        "second sweep must bring the list under budget"
    );
    assert!(removed > 0);
    assert_pairs_intact(&messages);
    // Surviving latest reads are byte-identical (removed wholesale, never
    // halved or summarized).
    for m in messages.iter().filter(|m| m.role == "tool") {
        if m.tool_call_id
            .as_deref()
            .is_some_and(|id| id.starts_with("r2_"))
        {
            assert_eq!(m.content, "S".repeat(2500));
        }
    }
}

#[test]
fn test_stale_read_marked_when_round_carries_protected_read() {
    // One round reads a.rs (small) and b.rs (large, current). a.rs is then
    // mutated. The trim must NOT delete the whole round (that would lose the
    // b.rs snapshot the model is working from): the stale a.rs result becomes
    // a marker in place, b.rs stays in full, and the age pass keeps the round
    // because of b.rs.
    let big_b = "B".repeat(5000);
    let mut messages = vec![user_msg("work")];
    for i in 0..10 {
        let cid = format!("junk_{i}");
        messages.push(assistant_tool_call(&cid, "{}"));
        messages.push(tool_result(&cid, &"j".repeat(900)));
    }
    messages.push(assistant_calls_multi(&[
        ("ra", "read_file", "{\"path\":\"a.rs\"}"),
        ("rb", "read_file", "{\"path\":\"b.rs\"}"),
    ]));
    messages.push(tool_result("ra", "old-a"));
    messages.push(tool_result("rb", &big_b));
    messages.push(assistant_call_named(
        "wa",
        "write_file",
        "{\"path\":\"a.rs\",\"content\":\"x\"}",
    ));
    messages.push(tool_result("wa", "ok"));
    messages.push(user_msg("go"));

    let target = 8000usize;
    assert!(
        ContextTrimming::message_char_count(&messages) > target,
        "precondition: must start over budget"
    );

    let trimming = ContextTrimming::new();
    trimming.trim_messages(&mut messages, target, &make_config());

    assert!(
        ContextTrimming::message_char_count(&messages) <= target,
        "after trim must be under budget"
    );
    assert_pairs_intact(&messages);
    assert_eq!(
        tool_content_at(&messages, "rb"),
        big_b,
        "the protected b.rs snapshot must survive the trim in full"
    );
    assert!(
        tool_content_at(&messages, "ra").starts_with("[read_file of "),
        "the stale a.rs read is marked in place, not removed"
    );
}
