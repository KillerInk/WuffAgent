//! Unit tests for the `registry` module (see `super`).

use super::*;
use crate::tools::builtin::{CalculationTool, ReadFileTool};
use crate::tools::types::{ToolLogger, TracingToolLogger};

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
