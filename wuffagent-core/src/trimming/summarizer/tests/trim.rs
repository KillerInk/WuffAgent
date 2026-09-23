//! Basic `trim_messages` behavior: protected tail, tool-pair unit removal,
//! last-user protection, budget convergence, no empty content.

use super::*;

#[test]
fn test_trim_keeps_tool_pair_straddling_boundary_intact() {
    // system, then an early user/assistant pair, then an assistant tool
    // call whose result immediately follows the last user message. The
    // trim target forces removals that land right on the call/result
    // boundary — the pair must survive together.
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
        user_msg("first question"),
        assistant_msg("first answer"),
        user_msg("second question"),
        assistant_tool_call("call_1", "{\"n\":42}"),
        tool_result("call_1", "42"),
        user_msg("third question"),
    ];

    let trimming = ContextTrimming::new();
    // Target just above the system+last-user cost so the removal loop runs
    // and its pointer reaches the tool cluster.
    let target = 60;
    trimming.trim_messages(&mut messages, target, &make_config());

    assert_pairs_intact(&messages);
    // The last user message must always survive.
    assert!(messages.iter().any(|m| m.content == "third question"));
}

#[test]
fn test_trim_multiple_user_messages_keeps_last_user_intact() {
    // Several user messages separated by assistant replies plus a tool
    // cluster near the end. Aggressive trimming must keep removing
    // messages (recomputing the last-user bound each iteration) and must
    // never drop the final user message.
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
        user_msg("q1"),
        assistant_msg("a1"),
        user_msg("q2"),
        assistant_msg("a2"),
        user_msg("q3"),
        assistant_tool_call("c1", "{}"),
        tool_result("c1", "ok"),
        user_msg("q4-final"),
    ];

    let trimming = ContextTrimming::new();
    // Target just below the total content but above the system + final
    // user floor: the removal loop must keep removing across multiple
    // user messages, recomputing the last-user bound each iteration, and
    // stop only once it would reach q4-final.
    let removed = trimming.trim_messages(&mut messages, 15, &make_config());

    assert!(removed > 0, "expected some messages to be removed");
    assert_pairs_intact(&messages);
    let last = messages.last().unwrap();
    assert_eq!(last.role, "user", "final message must stay a user message");
    assert_eq!(last.content, "q4-final");
}

#[test]
fn test_protected_tail_start_prefers_latest_round() {
    // Agent loop shape: one user turn, then several tool rounds. The
    // protected tail must start at the LAST assistant tool call, not at the
    // (much earlier) user message — otherwise every old tool result would
    // be protected and never summarized.
    let messages = vec![
        user_msg("q"),
        assistant_tool_call("c1", "{}"),
        tool_result("c1", "old-1"),
        assistant_tool_call("c2", "{}"),
        tool_result("c2", "old-2"),
        assistant_tool_call("c3", "{}"),
        tool_result("c3", "fresh"),
    ];
    // index 5 = the last assistant tool call
    assert_eq!(ContextTrimming::protected_tail_start(&messages), 5);
}

#[test]
fn test_protected_tail_start_plain_chat_uses_last_user() {
    let messages = vec![user_msg("q1"), assistant_msg("a1"), user_msg("q2")];
    assert_eq!(ContextTrimming::protected_tail_start(&messages), 2);
}

#[test]
fn test_fresh_tool_result_untouched_by_trim() {
    // The tool result of the current round (after the last assistant
    // tool call) must come back byte-identical, even when the older part
    // of the conversation is far over budget.
    let big = (0..500)
        .map(|i| format!("old line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let fresh = "FRESH-RESULT-EXACTLY-AS-IS";
    let mut messages = vec![
        user_msg("q"),
        assistant_tool_call("c1", "{}"),
        tool_result("c1", &big),
        assistant_tool_call("c2", "{}"),
        tool_result("c2", fresh),
    ];

    let trimming = ContextTrimming::new();
    // target tiny: forces removal/summarization of everything before the tail.
    trimming.trim_messages(&mut messages, 64, &make_config());

    let last = messages.last().unwrap();
    assert_eq!(last.role, "tool");
    assert_eq!(last.content, fresh, "fresh tool result must be untouched");
}

#[test]
fn test_old_tool_pair_removed_as_unit_when_over_budget() {
    // An over-budget tool round from an EARLIER turn is removed as a unit
    // (assistant-with-tool-calls + its tool results together), so pairing
    // stays intact and no orphaned tool result / dangling call remains.
    // The current round (last tool call + its fresh result) is protected.
    let big = (0..500)
        .map(|i| format!("old line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let fresh = "fresh";
    let mut messages = vec![
        user_msg("q"),
        assistant_tool_call("c1", "{}"),
        tool_result("c1", &big),
        assistant_tool_call("c2", "{}"),
        tool_result("c2", fresh),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 256, &make_config());

    assert!(removed > 0, "an over-budget old round must be removed");
    assert!(ContextTrimming::message_char_count(&messages) <= 256);
    // Pairing intact: no tool result without its call, no call without its result.
    assert_pairs_intact(&messages);
    // The old pair is gone; the current round's fresh result is intact.
    assert!(!messages
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert!(messages.iter().any(|m| m.role == "assistant"
        && m.tool_calls
            .as_ref()
            .map(|t| t.iter().any(|tc| tc.id == "c2"))
            .unwrap_or(false)));
    assert_eq!(messages.last().unwrap().content, fresh);
}

#[test]
fn test_tool_heavy_history_is_trimmed_under_budget() {
    // Regression: the real-world agent shape — a single leading user task
    // followed by a long run of tool pairs. The removal loop must NOT stop
    // at the first tool pair; it must drop consumed rounds until the budget
    // fits, keeping the protected tail (last tool call + fresh result) intact.
    let mut messages = vec![user_msg("do the thing")];
    for i in 0..40 {
        let cid = format!("call_{i}");
        let body = (0..200)
            .map(|l| format!("tool output line {l} for {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        messages.push(assistant_tool_call(&cid, "{\"k\":\"v\"}"));
        messages.push(tool_result(&cid, &body));
    }
    // The current round: a fresh tool call + its result.
    messages.push(assistant_tool_call("call_fresh", "{\"k\":\"v\"}"));
    messages.push(tool_result("call_fresh", "FRESH"));

    let initial = ContextTrimming::message_char_count(&messages);
    let target = 5000usize;
    assert!(
        initial > target,
        "precondition: must start over budget (got {initial})"
    );

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, target, &make_config());

    assert!(removed > 0, "tool-heavy history must have rounds removed");
    let final_chars = ContextTrimming::message_char_count(&messages);
    assert!(
        final_chars <= target,
        "after trim must be under budget (final={final_chars} target={target})"
    );
    assert_pairs_intact(&messages);
    // The fresh tail is preserved verbatim.
    assert_eq!(messages.last().unwrap().content, "FRESH");
    // The leading task is preserved.
    assert_eq!(messages.first().unwrap().content, "do the thing");
}

#[test]
fn test_no_empty_content_under_extreme_overage() {
    // With a target smaller than the system+tail floor, the fallback
    // truncation must run down to the placeholder floor — never to empty —
    // on the oversized non-tail messages.
    let big = "x".repeat(20_000);
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
        user_msg("q"),
        assistant_tool_call("c1", "{}"),
        tool_result("c1", &big),
        assistant_tool_call("c2", "{}"),
        tool_result("c2", "fresh"),
    ];

    let trimming = ContextTrimming::new();
    trimming.trim_messages(&mut messages, 100, &make_config());

    // No tool message may end up empty (that is what the agent/server
    // rejects). Assistant tool-call messages legitimately carry empty
    // content by design, so they are excluded from this check.
    for m in &messages {
        if m.role == "tool" {
            assert!(
                !m.content.is_empty(),
                "tool message ended up with empty content"
            );
        }
    }
    // The oversized old tool result must have been shrunk (it cannot fit).
    assert!(messages[3].content.len() < big.len());
    // Fresh result untouched.
    assert_eq!(messages.last().unwrap().content, "fresh");
}
