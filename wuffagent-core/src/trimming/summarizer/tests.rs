//! Unit tests for the `summarizer` module (see `super`).

use super::*;
use super::super::config::TrimConfig;

fn make_config() -> TrimConfig {
    TrimConfig {
        enabled: true,
        max_tool_result_chars: 200,
        max_chain_entries: 50,
        code_max_lines: 10,
        log_max_lines: 8,
        list_max_items: 10,
        stale_file_invalidation: true,
    }
}

#[test]
fn test_build_log_summarization() {
    let mut lines = Vec::new();
    for i in 0..50 {
        lines.push(format!("Compiling item {} ... ok", i));
    }
    let content = lines.join("\n");
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &make_config());

    assert!(result.contains("lines omitted"));
    assert!(result.len() <= 200);
    assert!(result.starts_with("Compiling item 0"));
    assert!(result.contains("Compiling item 49"));
}

#[test]
fn test_code_summarization() {
    let mut lines = Vec::new();
    for i in 0..100 {
        lines.push(format!("    let x{} = {};", i, i));
    }
    let content = lines.join("\n");
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &make_config());

    assert!(result.contains("lines omitted"));
    assert!(result.len() <= 200);
    assert!(result.starts_with("    let x0"));
}

#[test]
fn test_small_content_not_truncated() {
    let content = "short result";
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(content, &make_config());
    assert_eq!(result, "short result");
}

#[test]
fn test_disabled_config_returns_unmodified() {
    let content = "x".repeat(5000);
    let mut config = make_config();
    config.enabled = false;
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &config);
    assert_eq!(result.len(), 5000);
}

#[test]
fn test_file_list_summarization() {
    let paths: Vec<String> = (0..100)
        .map(|i| format!("/path/to/file_{}.rs", i))
        .collect();
    let content = paths.join("\n");
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &make_config());

    assert!(result.contains("items omitted"));
    assert!(result.len() <= 200);
}

fn user_msg(text: &str) -> Message {
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

fn assistant_msg(text: &str) -> Message {
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

fn assistant_tool_call(call_id: &str, args: &str) -> Message {
    assistant_call_named(call_id, "echo", args)
}

/// Assistant message requesting the named tool with the given raw args.
fn assistant_call_named(call_id: &str, tool_name: &str, args: &str) -> Message {
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

fn tool_result(call_id: &str, text: &str) -> Message {
    Message {
        role: "tool".into(),
        content: text.into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: Some(call_id.into()),
        reasoning_content: None,
        image: None,
    }
}

/// Every assistant tool-call id must have a matching tool result present in
/// the trimmed list, and every tool result's id must match a present call.
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
            "tool result {} present without its paired tool call",
            id
        );
    }
}

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
    let big = (0..500).map(|i| format!("old line {}", i)).collect::<Vec<_>>().join("\n");
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
    let big = (0..500).map(|i| format!("old line {}", i)).collect::<Vec<_>>().join("\n");
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
    assert!(!messages.iter().any(|m| m.tool_call_id.as_deref() == Some("c1")));
    assert!(messages.iter().any(|m| m.role == "assistant" && m.tool_calls.as_ref().map(|t| t.iter().any(|tc| tc.id == "c2")).unwrap_or(false)));
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
        let body = (0..200).map(|l| format!("tool output line {l} for {i}")).collect::<Vec<_>>().join("\n");
        messages.push(assistant_tool_call(&cid, "{\"k\":\"v\"}"));
        messages.push(tool_result(&cid, &body));
    }
    // The current round: a fresh tool call + its result.
    messages.push(assistant_tool_call("call_fresh", "{\"k\":\"v\"}"));
    messages.push(tool_result("call_fresh", "FRESH"));

    let initial = ContextTrimming::message_char_count(&messages);
    let target = 5000usize;
    assert!(initial > target, "precondition: must start over budget (got {initial})");

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

/// Content of the tool result paired with `call_id` (panics if missing).
fn tool_content_at<'a>(messages: &'a [Message], call_id: &str) -> &'a str {
    messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some(call_id))
        .map(|m| m.content.as_str())
        .expect("tool result missing")
}

// ── Freshness pass (stale/superseded read_file eviction) ─────────────────

#[test]
fn test_stale_read_evicted_before_age_removal() {
    // read A → write A → next turn: the old read is stale and must be
    // collapsed to a marker while the write pair and the tail stay intact.
    // Target is above the post-marker total, so the age-based loop must NOT
    // run — proving the marker alone brought the list under budget.
    let big = "L".repeat(5000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"src/a.rs\"}"),
        tool_result("c1", &big),
        assistant_call_named("c2", "write_file", "{\"path\":\"src/a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    let removed = trimming.trim_messages(&mut messages, 300, &make_config());

    assert_eq!(removed, 0, "marker alone must bring the list under budget");
    assert!(tool_content_at(&messages, "c1").contains("stale: file modified"));
    assert!(tool_content_at(&messages, "c1").starts_with("[read_file of src/a.rs"));
    assert_eq!(tool_content_at(&messages, "c2"), "ok", "write pair must be intact");
    assert!(messages.iter().any(|m| m.content == "q2"), "tail untouched");
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

    assert_eq!(removed, 0);
    assert!(tool_content_at(&messages, "c1").contains("superseded by a newer read"));
    assert_eq!(tool_content_at(&messages, "c2"), big2, "newest read stays in full");
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

    assert_eq!(removed, 0);
    assert!(tool_content_at(&messages, "c1").contains("stale: file modified"));
    assert_eq!(tool_content_at(&messages, "c2"), "ok");
    assert_eq!(tool_content_at(&messages, "c3"), r2, "post-mutation read is current");
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

    assert_eq!(removed, 0);
    assert!(tool_content_at(&messages, "c1").contains("stale: file modified"));
    assert_eq!(tool_content_at(&messages, "c3"), r2, "protected read must be untouched");
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
    assert_eq!(tool_content_at(&messages, "c1"), r1, "read must stay current");
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

    assert_eq!(removed, 0);
    assert_eq!(tool_content_at(&messages, "c1"), ra, "src read must stay current");
    assert!(tool_content_at(&messages, "c2").contains("stale: file modified"));
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
        !messages.iter().any(|m| m.content.starts_with("[read_file of ")),
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
        assistant_call_named("c2", "write_file", "{\"path\":\"src/a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let mut config = make_config();
    config.stale_file_invalidation = false;

    let trimming = ContextTrimming::new();
    // Target above the full (unmarked) total so the age loop never runs.
    let removed = trimming.trim_messages(&mut messages, 6000, &config);

    assert_eq!(removed, 0);
    assert_eq!(tool_content_at(&messages, "c1"), big, "legacy: stale read stays in full");
}

#[test]
fn test_trim_messages_idempotent_second_call_noop() {
    // A second trim over an already-marked list must change nothing.
    let big = "L".repeat(5000);
    let mut messages = vec![
        user_msg("q1"),
        assistant_call_named("c1", "read_file", "{\"path\":\"src/a.rs\"}"),
        tool_result("c1", &big),
        assistant_call_named("c2", "write_file", "{\"path\":\"src/a.rs\",\"content\":\"x\"}"),
        tool_result("c2", "ok"),
        user_msg("q2"),
    ];

    let trimming = ContextTrimming::new();
    assert_eq!(trimming.trim_messages(&mut messages, 300, &make_config()), 0);
    let marked = tool_content_at(&messages, "c1").to_string();
    assert!(marked.contains("stale: file modified"));

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
