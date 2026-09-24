//! Unit tests for the `manager` module (see `super`).

use super::*;
use crate::tools::registry::ToolEntry;
use crate::tools::types::ToolMetadata;

/// Build a ToolManager whose registry contains a single `shell` entry,
/// mirroring the global registration in `register_builtins`.
fn manager_with_shell() -> ToolManager {
    let registry = ToolRegistry::new(vec![], Arc::new(TracingToolLogger));
    registry
        .register(ToolEntry {
            tool: Arc::new(crate::tools::builtin::shell::ShellTool::new(
                crate::tools::builtin::shell::ShellConfig {
                    enabled: true,
                    ..Default::default()
                },
            )),
            metadata: ToolMetadata {
                name: "shell".to_string(),
                version: "1.0.0".to_string(),
                description: "Execute shell commands on the local system".to_string(),
                dependencies: vec![],
            },
            loaded_at: std::time::Instant::now(),
            plugin: None,
        })
        .unwrap();
    ToolManager::new(Arc::new(registry))
}

#[test]
fn test_without_shell_removes_shell_from_schema() {
    let tm = manager_with_shell();
    assert!(tm.get_allowed_tools().contains(&"shell".to_string()));

    let tm = tm.without_shell();
    let names = tm.get_allowed_tools();
    assert!(
        !names.contains(&"shell".to_string()),
        "shell should be removed from the schema: {:?}",
        names
    );
    assert!(tm.get_tool_definitions().is_empty());
}

#[test]
fn test_with_handoff_tool_swaps_entry() {
    use crate::tools::builtin::handoff::HandoffTool;
    let tm = manager_with_shell();
    assert!(
        !tm.get_allowed_tools().contains(&"handoff".to_string()),
        "fresh manager has no handoff tool"
    );

    let mailbox = Arc::new(std::sync::Mutex::new(None));
    let tool = HandoffTool::new(
        mailbox,
        std::path::PathBuf::from("does-not-matter"),
        Vec::new(),
        Vec::new(),
    );
    let tm = tm.with_handoff_tool(tool);

    let names = tm.get_allowed_tools();
    assert!(
        names.contains(&"handoff".to_string()),
        "handoff added: {:?}",
        names
    );
    assert!(
        names.contains(&"shell".to_string()),
        "other tools preserved: {:?}",
        names
    );
    // Exactly one handoff entry, and it is the per-execution one.
    let defs: Vec<_> = tm
        .get_tool_definitions()
        .into_iter()
        .filter(|d| d.function.name == "handoff")
        .collect();
    assert_eq!(defs.len(), 1);
}

/// Build an assistant message with the given (id, name, arguments) calls.
fn assistant_msg_with_calls(calls: Vec<(&str, &str, &str)>) -> Message {
    Message {
        role: "assistant".to_string(),
        content: String::new(),
        timestamp: String::new(),
        tool_calls: Some(
            calls
                .into_iter()
                .map(|(id, name, arguments)| crate::types::ToolCall {
                    id: id.to_string(),
                    call_type: "function".to_string(),
                    function: crate::types::ToolFunction {
                        name: name.to_string(),
                        arguments: arguments.to_string(),
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
fn test_tool_args_complete() {
    assert!(tool_args_complete(r#"{"path":"a.txt","content":"x"}"#));
    assert!(tool_args_complete("{}"));
    // Truncated mid-string — the shape left behind when the model's output
    // limit is hit inside an argument.
    assert!(!tool_args_complete(r#"{"content":"use super::state"#));
    // Valid JSON but not an object.
    assert!(!tool_args_complete(r#""just a string""#));
    assert!(!tool_args_complete("42"));
    // Empty or cut-off JSON.
    assert!(!tool_args_complete(""));
    assert!(!tool_args_complete("{"));
}

#[test]
fn test_repair_truncated_tool_calls_repairs_only_truncated() {
    let mut msg = assistant_msg_with_calls(vec![
        ("call_ok", "read_file", r#"{"path":"a.txt"}"#),
        ("call_trunc", "write_file", r#"{"content":"use super::state::ChatApp;"#),
        ("call_empty", "shell", ""),
    ]);
    let repaired = repair_truncated_tool_calls(&mut msg);
    assert_eq!(
        repaired,
        vec!["call_trunc".to_string(), "call_empty".to_string()],
        "only the incomplete calls are reported"
    );
    let calls = msg.tool_calls.as_ref().unwrap();
    assert_eq!(
        calls[0].function.arguments,
        r#"{"path":"a.txt"}"#,
        "complete calls are left untouched"
    );
    assert_eq!(
        calls[1].function.arguments,
        "{}",
        "truncated call is repaired to an empty object"
    );
    assert_eq!(calls[2].function.arguments, "{}");
}

#[test]
fn test_repair_truncated_tool_calls_noop_without_calls() {
    let mut msg = assistant_msg_with_calls(Vec::new());
    msg.tool_calls = None;
    assert!(repair_truncated_tool_calls(&mut msg).is_empty());
}
