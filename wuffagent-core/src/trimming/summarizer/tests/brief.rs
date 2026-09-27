//! The mission-brief re-insertion (autoplans/context-rot-prevention.md, S1):
//! after the age-based removals, the rolling session brief is anchored right
//! after the system prompt, updated in place (never stacked) on later trims,
//! carries the dropped task/corrections/decisions/file mutations, and is not
//! inserted for a trivially small drop.

use super::*;
use crate::trimming::brief;

fn system_msg() -> Message {
    Message {
        role: "system".into(),
        content: "You are a helpful agent.".into(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

/// One "already consumed" echo round (~810 chars).
fn junk_round(messages: &mut Vec<Message>, i: usize) {
    let cid = format!("junk_{i}");
    messages.push(assistant_tool_call(&cid, &format!("{{\"n\":{i}}}")));
    messages.push(tool_result(&cid, &"j".repeat(800)));
}

fn briefs_in(messages: &[Message]) -> Vec<&Message> {
    messages.iter().filter(|m| brief::is_brief_message(m)).collect()
}

#[test]
fn brief_anchored_with_task_and_correction_after_big_drop() {
    let task = "Refactor the trimming module and keep the tests green";
    let mut messages = vec![system_msg(), user_msg(task)];
    for i in 0..12 {
        junk_round(&mut messages, i);
    }
    messages.push(user_msg("no, use the new API, not the old one"));
    messages.push(user_msg("go"));
    messages.push(assistant_tool_call("t1", "{}"));
    messages.push(tool_result("t1", &"R".repeat(2000)));

    let target = 4500usize;
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
    // The verbatim task survives (the age sweep skips the first user message).
    assert!(
        messages
            .iter()
            .any(|m| m.role == "user" && m.content == task),
        "the task message must survive the sweep"
    );
    // Exactly one brief, anchored right after the verbatim task.
    assert_eq!(briefs_in(&messages).len(), 1, "exactly one mission brief");
    let pos = messages
        .iter()
        .position(|m| brief::is_brief_message(m))
        .expect("brief present");
    assert_eq!(
        messages[pos - 1].content, task,
        "brief anchored right after the verbatim task"
    );
    let text = &messages[pos].content;
    assert!(text.contains(task), "brief carries the task");
    assert!(
        text.contains("compacted"),
        "the model is told compaction happened"
    );
}

#[test]
fn brief_updated_rolling_not_stacked_on_second_trim() {
    let task = "Refactor the trimming module and keep the tests green";
    let correction = "no, use the new API, not the old one";
    let mut messages = vec![system_msg(), user_msg(task)];
    for i in 0..12 {
        junk_round(&mut messages, i);
    }
    messages.push(user_msg(correction));
    messages.push(user_msg("go"));
    messages.push(assistant_tool_call("t1", "{}"));
    messages.push(tool_result("t1", &"R".repeat(200)));

    let trimming = ContextTrimming::new();
    trimming.trim_messages(&mut messages, 4500, &make_config());

    // Second round: the model decides something, writes a file, adds a junk
    // round, and the user replies.
    messages.push(assistant_msg(
        "I decided to keep the settings dialog for v2.\n\
         - created panel.rs\n\
         Next: wire the events.",
    ));
    messages.push(assistant_call_named("w1", "write_file", "{\"path\":\"panel.rs\"}"));
    messages.push(tool_result("w1", "ok"));
    junk_round(&mut messages, 99);
    messages.push(user_msg("ship it"));
    messages.push(assistant_tool_call("t2", "{}"));
    messages.push(tool_result("t2", &"R".repeat(2000)));

    let target = 3000usize;
    assert!(
        ContextTrimming::message_char_count(&messages) > target,
        "precondition: second trim must start over budget"
    );

    trimming.trim_messages(&mut messages, target, &make_config());

    assert!(
        ContextTrimming::message_char_count(&messages) <= target,
        "under budget after the second trim"
    );
    assert_pairs_intact(&messages);
    // The brief is updated in place, never stacked.
    assert_eq!(briefs_in(&messages).len(), 1, "brief updated, not stacked");
    let pos = messages
        .iter()
        .position(|m| brief::is_brief_message(m))
        .expect("brief present");
    assert_eq!(messages[pos - 1].content, task, "brief still anchored after the task");
    let text = &messages[pos].content;
    // Facts from the FIRST trim survive (re-parsed from the old render).
    assert!(text.contains(task), "task survives the rolling update");
    assert!(
        text.contains(correction),
        "the correction dropped in the first trim survives"
    );
    // Facts from the SECOND dropped span are folded in.
    assert!(
        text.contains("decided to keep the settings dialog"),
        "new decision captured"
    );
    assert!(text.contains("created panel.rs"), "new done-line captured");
    assert!(
        text.contains("panel.rs (write_file)"),
        "file mutation of the dropped round captured"
    );
}

#[test]
fn no_brief_when_the_dropped_span_is_trivial() {
    let mut messages = vec![system_msg(), user_msg("small task")];
    junk_round(&mut messages, 0);
    messages.push(user_msg("go"));
    messages.push(assistant_tool_call("t1", "{}"));
    messages.push(tool_result("t1", &"R".repeat(2000)));

    let target = 2500usize;
    assert!(
        ContextTrimming::message_char_count(&messages) > target,
        "precondition"
    );

    let trimming = ContextTrimming::new();
    trimming.trim_messages(&mut messages, target, &make_config());

    // Only ~810 chars were dropped (< BRIEF_MIN_DROPPED_CHARS): the brief
    // would cost a comparable share of the savings, so none is inserted.
    assert!(briefs_in(&messages).is_empty(), "no brief for a trivial drop");
    assert_pairs_intact(&messages);
    assert!(
        ContextTrimming::message_char_count(&messages) <= target,
        "under budget"
    );
}
