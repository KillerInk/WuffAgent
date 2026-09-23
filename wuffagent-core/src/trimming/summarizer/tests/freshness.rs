//! Freshness pass (stale/superseded read_file eviction), last-resort
//! shrinking, and the "file content is all-or-nothing" invariants.

use super::*;

#[test]
fn test_stale_read_pair_removed_wholesale() {
    // read A → write A → next turn: the stale read's WHOLE tool pair
    // (assistant call + result) is removed — no marker, no partial content —
    // while the write pair and the tail stay intact. Target is above the
    // post-removal total, so the age-based loop must NOT run — proving the
    // pair removal alone brought the list under budget.
    let big = "L".repeat(5000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"src/a.rs\"}"),
        tool_result("c1", &big),
        assistant_call_named(
            "c2",
            "write_file",
            "{\"path\":\"src/a.rs\",\"content\":\"x\"}",
        ),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 300, &make_config());

    assert_eq!(
        removed, 2,
        "stale read pair (assistant + tool) must be removed"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("c1")),
        "stale read result must be gone"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.content.starts_with("[read_file of ")),
        "no marker may remain — the pair is removed, not marked"
    );
    assert_eq!(
        tool_content_at(&messages, "c2"),
        "ok",
        "write pair must be intact"
    );
    assert!(messages.iter().any(|m| m.content == "q2"), "tail untouched");
    assert_pairs_intact(&messages);
}

#[test]
fn test_marker_pair_removed_on_next_trim() {
    // A persisted session may already contain a one-line marker (legacy
    // marker form). The next trim must drop that pair entirely instead of
    // keeping the stub around forever.
    let marker =
        "[read_file of a.rs — stale: file modified after this read; re-read before relying on it]";
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", marker),
        assistant_call_named("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 300, &make_config());

    assert_eq!(removed, 2, "marker pair must be removed");
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert_eq!(tool_content_at(&messages, "c2"), "ok");
    assert_pairs_intact(&messages);
}

#[test]
fn test_superseded_read_evicted() {
    // read A → read A (no mutation): the older read is redundant; the
    // newest read is the current content and stays in full.
    let big1 = "A".repeat(3000);
    let big2 = "B".repeat(3000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &big1),
        assistant_call_named("c2", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c2", &big2),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 4000, &make_config());

    assert_eq!(removed, 2, "superseded read pair must be removed");
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert_eq!(
        tool_content_at(&messages, "c2"),
        big2,
        "newest read stays in full"
    );
    assert_pairs_intact(&messages);
}

#[test]
fn test_read_write_reread_keeps_latest_snapshot() {
    // read A → write A → read A: first read stale, second read current.
    let r1 = "R1".repeat(500);
    let r2 = "R2".repeat(500);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &r1),
        assistant_call_named("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "ok"),
        assistant_call_named("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", &r2),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 2000, &make_config());

    assert_eq!(removed, 2, "stale first read pair must be removed");
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert_eq!(tool_content_at(&messages, "c2"), "ok");
    assert_eq!(
        tool_content_at(&messages, "c3"),
        r2,
        "post-mutation read is current"
    );
}

#[test]
fn test_stale_read_in_protected_tail_untouched() {
    // Agent shape ending mid-round: the last assistant tool call starts the
    // protected tail, so a stale read INSIDE the tail must stay in full.
    let r1 = "R1".repeat(500);
    let r2 = "R2".repeat(500);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &r1),
        assistant_call_named("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "ok"),
        assistant_call_named("c3", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c3", &r2),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 2000, &make_config());

    assert_eq!(removed, 2, "stale pre-tail read pair must be removed");
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert_eq!(
        tool_content_at(&messages, "c3"),
        r2,
        "protected read must be untouched"
    );
}

#[test]
fn test_failed_mutation_does_not_invalidate_read() {
    // A write_file that failed ("Error:" prefix) did not change the file,
    // so the earlier read is still current.
    let r1 = "S".repeat(1000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &r1),
        assistant_call_named("c2", "write_file", "{\"path\":\"a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "Error: disk full"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 2000, &make_config());

    assert_eq!(removed, 0);
    assert_eq!(
        tool_content_at(&messages, "c1"),
        r1,
        "read must stay current"
    );
}

#[test]
fn test_copy_only_invalidates_dest() {
    // copy A→B: reads of B (dest) go stale; reads of A (src) stay current.
    let ra = "C".repeat(1000);
    let rb = "D".repeat(1000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &ra),
        assistant_call_named("c2", "read_file", "{\"path\":\"b.rs\"}"),
        tool_result("c2", &rb),
        assistant_call_named("c3", "copy", "{\"src\":\"a.rs\",\"dest\":\"b.rs\"}"),
        tool_result("c3", "ok"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 2000, &make_config());

    assert_eq!(removed, 2, "stale dest read pair must be removed");
    assert_eq!(
        tool_content_at(&messages, "c1"),
        ra,
        "src read must stay current"
    );
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c2")));
}

#[test]
fn test_plain_chat_no_tool_messages_noop() {
    // No tool messages: the freshness pass is a no-op and age-based
    // trimming behaves exactly as before.
    let mut messages = vec![
        user_msg(&format!("q1{}", "x".repeat(200))),
        assistant_msg(&format!("a1{}", "y".repeat(200))),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 50, &make_config());

    assert_eq!(removed, 2, "age-based removal must still run");
    assert!(
        !messages
            .iter()
            .any(|m| m.content.starts_with("[read_file of ")),
        "no markers may appear without tool messages"
    );
    assert_eq!(messages.last().unwrap().content, "q2");
}

#[test]
fn test_flag_off_keeps_legacy_behavior() {
    // stale_file_invalidation = false: the stale read stays in full and the
    // output is identical to the legacy trim.
    let big = "L".repeat(5000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"src/a.rs\"}"),
        tool_result("c1", &big),
        assistant_call_named(
            "c2",
            "write_file",
            "{\"path\":\"src/a.rs\",\"content\":\"x\"}",
        ),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let mut config = make_config();
    config.stale_file_invalidation = false;

    let trimming = ContextTrimming::new();
    // Target above the full (unmarked) total so the age loop never runs.
    let removed = trimming.trim_messages(&mut messages, 6000, &config);

    assert_eq!(removed, 0);
    assert_eq!(
        tool_content_at(&messages, "c1"),
        big,
        "legacy: stale read stays in full"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.content.starts_with("[read_file of ")),
        "legacy mode must not produce markers either"
    );
}

#[test]
fn test_trim_messages_idempotent_second_call_noop() {
    // A second trim over an already-marked list must change nothing.
    let big = "L".repeat(5000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"src/a.rs\"}"),
        tool_result("c1", &big),
        assistant_call_named(
            "c2",
            "write_file",
            "{\"path\":\"src/a.rs\",\"content\":\"x\"}",
        ),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    // First trim: the stale read pair is removed wholesale.
    assert_eq!(
        trimming.trim_messages(&mut messages, 300, &make_config()),
        2,
        "stale read pair must be removed on the first trim"
    );
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));

    let before: Vec<(String, String)> = messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();
    let removed = trimming.trim_messages(&mut messages, 300, &make_config());
    let after: Vec<(String, String)> = messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    assert_eq!(removed, 0, "second trim must remove nothing");
    assert_eq!(before, after, "second trim must not modify the list");
}

#[test]
fn test_trim_shrinks_huge_fresh_tool_result_last_resort() {
    // The fresh round (last assistant tool call + its result) alone exceeds
    // the target; almost nothing pre-tail can be removed or summarized.
    // Without the last-resort tail shrink the list stays over budget and the
    // next request overflows n_ctx; with it, the tool result is halved down.
    let mut messages = vec![
        Message {
            role: "system".into(),
            content: "sys".into(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        },
        user_msg("read the big file"),
        assistant_tool_call("call_1", "{\"path\":\"big.txt\"}"),
        tool_result("call_1", &"x".repeat(50_000)),
    ];

    let trimming = ContextTrimming::new();
    let target = 1_000;
    trimming.trim_messages(&mut messages, target, &make_config());

    assert_pairs_intact(&messages);
    assert_eq!(messages.len(), 4, "nothing should be removed");
    let total = ContextTrimming::message_char_count(&messages);
    assert!(
        total <= target,
        "tail shrink must bring the list under budget (total={total}, target={target})"
    );
    assert!(!messages[3].content.is_empty(), "tool result must not be emptied");
}

#[test]
fn test_trim_last_resort_shrinks_fresh_read_file() {
    // A fresh read_file result inside the protected tail is exempt from the
    // normal shrinkers (file content is all-or-nothing), but when the tail
    // alone exceeds the budget the last-resort stage must halve it anyway —
    // removing the pair would orphan the live tool call.
    let mut messages = vec![
        user_msg("read the file"),
        assistant_call_named("call_1", "read_file", "{\"path\":\"a.txt\"}"),
        tool_result("call_1", &"line of content\n".repeat(30_000)),
    ];

    let trimming = ContextTrimming::new();
    let target = 2_000;
    trimming.trim_messages(&mut messages, target, &make_config());

    assert_pairs_intact(&messages);
    assert_eq!(messages.len(), 3, "nothing should be removed");
    let total = ContextTrimming::message_char_count(&messages);
    assert!(
        total <= target,
        "read_file tail must be halved under budget (total={total}, target={target})"
    );
}

#[test]
fn test_trim_tail_shrink_never_destroys_short_user_request() {
    // When nothing can fit the budget (an oversized system prompt eats the
    // whole window), the last-resort tail shrink must leave a SHORT user
    // request untouched — collapsing it to the 41-char placeholder would
    // grow the total instead of shrinking it.
    let mut messages = vec![
        Message {
            role: "system".into(),
            content: "S".repeat(5_000),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        },
        user_msg("the current request"),
    ];

    let trimming = ContextTrimming::new();
    trimming.trim_messages(&mut messages, 1_000, &make_config());

    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[1].content, "the current request",
        "the fresh user request must survive verbatim"
    );
    assert_eq!(
        messages[0].content, "S".repeat(5_000),
        "the leading system prompt must never be shrunk"
    );
}

// ── Never leave a partial file snapshot ────────────────────────────────────

#[test]
fn test_summarize_skips_read_file_results_when_flag_on() {
    // With freshness eviction on, an over-budget pre-tail read_file result is
    // NEVER summarized in place (a partial, line-numbered snapshot invites the
    // model to hallucinate line contents it no longer has) — other tool
    // results in the same list still are.
    let big = (0..100)
        .map(|i| format!("{:4} | let x{i} = {i};", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut messages = vec![
        user_msg("q"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &big),
        assistant_call_named("c2", "shell", "{\"command\":\"cargo build\"}"),
        tool_result("c2", &big),
        user_msg("q2"),
    ];
    // c1 is a current read (no mutation, no newer read) → survives the
    // freshness pass; the tail starts at the last user message (index 5).
    let trimming = ContextTrimming::new();
    trimming.summarize_old_tool_messages(&mut messages, 5, 100, &make_config(), true);

    assert_eq!(
        tool_content_at(&messages, "c1"),
        big,
        "read_file result must stay in full"
    );
    assert_ne!(
        tool_content_at(&messages, "c2"),
        big,
        "shell result must still be summarized"
    );
}

#[test]
fn test_summarize_still_shrinks_reads_when_flag_off() {
    // Legacy mode: the in-place summarizer applies to read_file results too.
    let big = (0..100)
        .map(|i| format!("{:4} | let x{i} = {i};", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut messages = vec![
        user_msg("q"),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &big),
        user_msg("q2"),
    ];
    let trimming = ContextTrimming::new();
    trimming.summarize_old_tool_messages(&mut messages, 3, 100, &make_config(), false);

    assert_ne!(
        tool_content_at(&messages, "c1"),
        big,
        "legacy: read result must be summarized"
    );
}

#[test]
fn test_truncate_never_halves_read_file_result_when_flag_on() {
    // The last-resort halver must skip read_file results (flag on) so file
    // content is either fully present or fully absent — while other messages
    // are still halved down to the placeholder floor.
    let big = (0..100)
        .map(|i| format!("{:4} | let x{i} = {i};", i))
        .collect::<Vec<_>>()
        .join("\n");
    let prose = "y".repeat(4000);
    let mut messages = vec![
        user_msg(&prose),
        assistant_call_named("c1", "read_file", "{\"path\":\"a.rs\"}"),
        tool_result("c1", &big),
        user_msg("q2"),
    ];
    // Tail starts at the last user message (index 3).
    let did = ContextTrimming::truncate_largest_message(&mut messages, 100, 3, true, 0);

    assert!(did, "something must have been truncated");
    assert_eq!(
        tool_content_at(&messages, "c1"),
        big,
        "read_file result must not be halved"
    );
    assert!(
        messages[0].content.len() < prose.len(),
        "the oversized non-read message must be shrunk"
    );
}

#[test]
fn test_current_read_pair_removed_by_age_loop_stays_wholesale() {
    // End-to-end: a long tool-heavy history where the reads are CURRENT
    // (no writes). Age-based removal must drop whole read pairs — after the
    // trim, no read_file result in the list may be a partial snapshot:
    // surviving reads are byte-identical to what was produced.
    let mut messages = vec![user_msg("do the thing")];
    let snapshots: Vec<String> = (0..20)
        .map(|i| {
            (0..120)
                .map(|l| format!("{:4} | code line {l} of file {i}", i))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    for (i, snap) in snapshots.iter().enumerate() {
        let cid = format!("call_{i}");
        messages.push(assistant_call_named(
            &cid,
            "read_file",
            &format!("{{\"path\":\"f{}.rs\"}}", i),
        ));
        messages.push(tool_result(&cid, snap));
    }
    // The current round: a fresh read + its result (protected tail).
    messages.push(assistant_call_named(
        "call_fresh",
        "read_file",
        "{\"path\":\"fresh.rs\"}",
    ));
    messages.push(tool_result("call_fresh", "FRESH-CONTENT"));

    let target = 5000usize;
    assert!(
        ContextTrimming::message_char_count(&messages) > target,
        "precondition: must start over budget"
    );

    let trimming = ContextTrimming::new();
    trimming.trim_messages(&mut messages, target, &make_config());

    assert_pairs_intact(&messages);
    assert!(
        ContextTrimming::message_char_count(&messages) <= target,
        "after trim must be under budget"
    );
    // No marker stubs and no truncated (mid-line cut) snapshots either:
    // a surviving read either equals its original snapshot or is gone.
    for m in messages.iter().filter(|m| m.role == "tool") {
        if m.tool_call_id.as_deref() == Some("call_fresh") {
            continue;
        }
        let Some(i) = m
            .tool_call_id
            .as_deref()
            .and_then(|id| id.strip_prefix("call_"))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        assert_eq!(
            m.content, snapshots[i],
            "surviving read snapshot must be byte-identical, never partial"
        );
    }
}
