//! Unit tests for the `manager` module (see `super`).

use super::*;
use crate::tools::types::ToolMetadata;
use crate::tools::registry::ToolEntry;

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
    assert!(names.contains(&"handoff".to_string()), "handoff added: {:?}", names);
    assert!(names.contains(&"shell".to_string()), "other tools preserved: {:?}", names);
    // Exactly one handoff entry, and it is the per-execution one.
    let defs: Vec<_> = tm
        .get_tool_definitions()
        .into_iter()
        .filter(|d| d.function.name == "handoff")
        .collect();
    assert_eq!(defs.len(), 1);
}
