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

// ─── Schema validation gate (T5/M2) ─────────────────────────────────────────

/// A trivial tool that counts how often it actually ran, and declares a
/// schema requiring an integer `count` field.
struct CountingTool {
    runs: Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::tools::types::Tool for CountingTool {
    fn name(&self) -> &str {
        "counting"
    }
    fn description(&self) -> &str {
        "counts how often it runs"
    }
    fn parameters_schema(&self) -> crate::tools::types::ToolSchema {
        use crate::tools::types::{FieldSchema, JsonSchema, ToolSchema};
        let mut props = std::collections::HashMap::new();
        props.insert(
            "count".to_string(),
            FieldSchema {
                description: "how many".to_string(),
                type_name: "integer".to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "counting".to_string(),
            description: "counts how often it runs".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                required: vec!["count".to_string()],
                properties: Some(props),
            }),
        }
    }
    fn execute(&self, _params: ToolParams) -> ToolResult<ToolOutput> {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput::success("ok"))
    }
}

fn manager_with_counting(runs: Arc<std::sync::atomic::AtomicUsize>) -> ToolManager {
    let registry = ToolRegistry::new(vec![], Arc::new(TracingToolLogger));
    registry
        .register(ToolEntry {
            tool: Arc::new(CountingTool { runs }),
            metadata: ToolMetadata {
                name: "counting".to_string(),
                version: "1.0.0".to_string(),
                description: "counts how often it runs".to_string(),
                dependencies: vec![],
            },
            loaded_at: std::time::Instant::now(),
            plugin: None,
        })
        .unwrap();
    ToolManager::new(Arc::new(registry))
}

#[tokio::test]
async fn test_execute_rejects_params_failing_schema_validation() {
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tm = manager_with_counting(runs.clone());

    // Missing required field → InvalidParams, tool never ran.
    let err = tm.execute("counting", ToolParams::new()).await.unwrap_err();
    match &err {
        ToolError::InvalidParams(msg) => {
            assert!(msg.contains("missing required parameter 'count'"), "{msg}")
        }
        other => panic!("expected InvalidParams, got {other:?}"),
    }
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Wrong type → InvalidParams, tool never ran.
    let mut p = ToolParams::new();
    p.values
        .insert("count".to_string(), serde_json::json!("three"));
    let err = tm.execute("counting", p).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)), "{err:?}");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Valid params pass validation and the tool runs.
    let mut p = ToolParams::new();
    p.values.insert("count".to_string(), serde_json::json!(2));
    tm.execute("counting", p)
        .await
        .expect("valid params must pass validation");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_validate_reports_all_problems_and_is_ok_for_schemaless_tools() {
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tm = manager_with_counting(runs);

    // Wrong type → InvalidParams naming the problem (and nothing else).
    let mut p = ToolParams::new();
    p.values.insert("count".to_string(), serde_json::json!(1.5));
    let err = tm.validate("counting", &p).unwrap_err();
    assert!(err.to_string().contains("expected integer"), "{err:?}");
    assert!(!err.to_string().contains("missing required"), "{err:?}");

    // Unknown tool → validation passes (no schema to check against).
    assert!(tm.validate("does-not-exist", &ToolParams::new()).is_ok());
}
