//! Unit tests for the `summarizer` module (see `super`).

use super::super::config::TrimConfig;
use super::*;

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

// ── Protected current reads survive the age-based removal ──────────────────

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
