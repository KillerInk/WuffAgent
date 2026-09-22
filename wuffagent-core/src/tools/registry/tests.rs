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
