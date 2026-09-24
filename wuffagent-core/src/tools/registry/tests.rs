//! Unit tests for the `registry` module (see `super`).

use super::*;
use crate::tools::builtin::{CalculationTool, ReadFileTool};
use crate::tools::types::{ToolLogger, ToolOutput, ToolParams, TracingToolLogger};

fn mock_logger() -> Arc<dyn ToolLogger> {
    Arc::new(TracingToolLogger)
}

fn make_entry(tool: Arc<dyn Tool>) -> ToolEntry {
    ToolEntry {
        tool,
        metadata: ToolMetadata {
            name: "test_tool".to_string(),
            version: "1.0.0".to_string(),
            description: "A test tool".to_string(),
            dependencies: vec![],
        },
        loaded_at: Instant::now(),
        plugin: None,
    }
}

#[test]
fn test_register_and_get_tool() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let tool = Arc::new(CalculationTool::new());
    let name = tool.name().to_string();
    registry.register(make_entry(tool)).unwrap();

    let retrieved = registry.get(&name);
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().name(), name);
}

#[test]
fn test_register_duplicate_tool_fails() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let tool = Arc::new(CalculationTool::new());
    registry.register(make_entry(tool.clone())).unwrap();

    let result = registry.register(make_entry(tool));
    assert!(result.is_err());
}

#[test]
fn test_unregister_tool() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let tool = Arc::new(CalculationTool::new());
    let name = tool.name().to_string();
    registry.register(make_entry(tool)).unwrap();

    let result = registry.unregister(&name);
    assert!(result.is_ok());
    assert!(registry.get(&name).is_none());
}

#[test]
fn test_unregister_missing_tool_fails() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let result = registry.unregister("nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_list_tools_empty() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert_eq!(registry.list().len(), 0);
}

#[test]
fn test_list_tools_after_adds() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    registry
        .register(make_entry(Arc::new(CalculationTool::new())))
        .unwrap();
    registry
        .register(make_entry(Arc::new(ReadFileTool::new())))
        .unwrap();

    assert_eq!(registry.list().len(), 2);
}

#[test]
fn test_list_schemas() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert!(registry.list_schemas().is_empty());

    registry
        .register(make_entry(Arc::new(CalculationTool::new())))
        .unwrap();
    let schemas = registry.list_schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "calculation");
}

#[test]
fn test_to_tool_definitions() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert!(registry.to_tool_definitions().is_empty());

    registry
        .register(make_entry(Arc::new(CalculationTool::new())))
        .unwrap();
    let definitions = registry.to_tool_definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].function.name, "calculation");
    assert_eq!(definitions[0].type_name, "function");
}

// ─── T3b: discovery paths ────────────────────────────────────────────────────

#[test]
fn test_add_discovery_path_idempotent() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let p = PathBuf::from("/tmp/wa-test-plugins");
    assert!(registry.add_discovery_path(p.clone()), "first add is new");
    assert!(!registry.add_discovery_path(p.clone()), "second add is a no-op");
    assert_eq!(registry.discovery_paths(), vec![p]);
}

#[test]
fn test_discovery_paths_seeded_at_construction() {
    let seeded = PathBuf::from("/seeded");
    let registry = ToolRegistry::new(vec![seeded.clone()], mock_logger());
    assert_eq!(registry.discovery_paths(), vec![seeded]);
}

#[test]
fn test_discover_plugins_empty_dir_loads_nothing() {
    let dir = std::env::temp_dir().join(format!("wa-reg-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert!(outcomes.is_empty(), "empty dir has no plugin files: {:?}", outcomes);
    assert_eq!(registry.discover_plugins().unwrap(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_discover_plugins_ignores_non_plugin_files() {
    let dir = std::env::temp_dir().join(format!("wa-reg-ign-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a plugin").unwrap();
    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert!(outcomes.is_empty(), "non-.dll/.so files are skipped: {:?}", outcomes);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_discover_plugins_broken_dll_reports_failed_not_err() {
    let dir = std::env::temp_dir().join(format!("wa-reg-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bogus.dll"), b"definitely not a PE file").unwrap();
    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert_eq!(outcomes.len(), 1, "one file scanned");
    assert_eq!(outcomes[0].status, PluginLoadStatus::Failed);
    assert!(outcomes[0].error.as_ref().unwrap().contains("bogus.dll"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_plugin_tool_usable_after_discover_returns() {
    // Regression test for the use-after-unload crash: `load_one_plugin` used
    // to drop the `PluginHandle` (owner of the `Library`) when it returned,
    // which unloaded the DLL while the registered `Arc<dyn Tool>` still
    // carried the plugin's vtable. The next vtable call — e.g.
    // `to_tool_definitions` when the agent loads its tool set — crashed with
    // STATUS_ACCESS_VIOLATION.
    let manifest = match std::env::var("CARGO_MANIFEST_DIR").ok() {
        Some(m) => m,
        None => return,
    };
    let dll = std::path::Path::new(&manifest)
        .parent()
        .unwrap()
        .join("target/debug/hello_plugin.dll");
    if !dll.exists() {
        eprintln!("skipping: hello_plugin.dll not built (cargo build -p hello_plugin)");
        return;
    }
    // Copy the DLL into a scratch dir so the scan only ever sees it.
    let dir = std::env::temp_dir().join(format!("wa-reg-hello-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&dll, dir.join("hello_plugin.dll")).unwrap();

    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, PluginLoadStatus::Loaded);
    assert_eq!(registry.list().len(), 1);

    // `discover_plugins` has returned — the handle that loaded the DLL is
    // gone from the loader's scope. Every call below goes through the
    // plugin's vtable and must still work.
    let tool = registry.get("hello").expect("hello registered");
    assert_eq!(tool.name(), "hello");
    assert_eq!(tool.parameters_schema().name, "hello");
    match tool.execute(ToolParams::default()) {
        Ok(ToolOutput::Success(_)) => {}
        other => panic!("hello execute failed: {other:?}"),
    }
    assert_eq!(registry.to_tool_definitions().len(), 1);

    // Drop the local tool clone FIRST (its Arc's drop glue lives in the DLL),
    // then the registry (entry + keepalive handle → FreeLibrary), and only
    // then remove the dir (the DLL file is locked while mapped).
    drop(tool);
    drop(registry);
    let _ = std::fs::remove_dir_all(&dir);
}
