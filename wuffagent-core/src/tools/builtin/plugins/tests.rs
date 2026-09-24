//! Unit tests for the `plugins` tools (T3b).

use super::*;
use crate::tools::registry::ToolEntry;
use crate::tools::types::{ToolMetadata, TracingToolLogger};

fn registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)))
}

fn register_calc(reg: &ToolRegistry) {
    reg.register(ToolEntry {
        tool: Arc::new(crate::tools::builtin::CalculationTool::new()),
        metadata: ToolMetadata {
            name: "calculation".to_string(),
            version: "1.0.0".to_string(),
            description: "calc".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })
    .unwrap();
}

#[test]
fn test_reload_plugins_empty_dir_reports_nothing() {
    let reg = registry();
    let dir = std::env::temp_dir().join(format!("wa-plugins-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    reg.add_discovery_path(dir.clone());

    let tool = ReloadPluginsTool::new(reg.clone());
    let out = tool.execute(Default::default()).unwrap();
    if let ToolOutput::Success(v) = &out {
        let v = v.as_object().unwrap();
        assert_eq!(v["loaded"].as_array().unwrap().len(), 0);
        assert_eq!(v["failed"].as_array().unwrap().len(), 0);
        assert_eq!(v["skipped"].as_array().unwrap().len(), 0);
        assert!(v["discovery_paths"].as_array().unwrap().len() >= 1);
    } else {
        panic!("expected success output");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_add_plugin_path_idempotent_and_reports_state() {
    let reg = registry();
    let dir = std::env::temp_dir().join(format!("wa-plugins-add-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let tool = AddPluginPathTool::new(reg.clone());
    let first = tool
        .execute(ToolParams {
            values: std::collections::HashMap::from([
                ("path".to_string(), serde_json::json!(dir.display().to_string())),
            ]),
        })
        .unwrap();
    if let ToolOutput::Success(v) = &first {
        assert_eq!(v["status"], "added");
    } else {
        panic!("expected success output");
    }

    let second = tool
        .execute(ToolParams {
            values: std::collections::HashMap::from([
                ("path".to_string(), serde_json::json!(dir.display().to_string())),
            ]),
        })
        .unwrap();
    if let ToolOutput::Success(v) = &second {
        assert_eq!(v["status"], "already_present");
    } else {
        panic!("expected success output");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_add_plugin_path_requires_path() {
    let reg = registry();
    let tool = AddPluginPathTool::new(reg);
    let res = tool.execute(Default::default());
    assert!(matches!(res, Err(ToolError::InvalidParams(_))));
}

#[test]
fn test_register_plugin_tools_registers_both() {
    let reg = ToolRegistry::new(vec![], Arc::new(TracingToolLogger));
    register_calc(&reg); // make the registry non-empty like the real app
    let arc = Arc::new(reg);
    register_plugin_tools(&arc, arc.clone()).unwrap();
    let names = arc
        .list()
        .iter()
        .map(|e| e.metadata.name.clone())
        .collect::<std::collections::HashSet<_>>();
    assert!(names.contains("reload_plugins"));
    assert!(names.contains("add_plugin_path"));
    assert!(names.contains("calculation"));
}
