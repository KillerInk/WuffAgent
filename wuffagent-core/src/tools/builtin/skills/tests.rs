//! Tests for the skill tools (K1): save/list/read/delete round-trip and
//! error paths.

use super::*;
use crate::memory::skills::SkillStore;
use crate::tools::types::ToolParams;
use std::sync::Arc;

fn tool_params(json: serde_json::Value) -> ToolParams {
    let values = json
        .as_object()
        .expect("params must be an object")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    ToolParams { values }
}

fn temp_store(tag: &str) -> Arc<SkillStore> {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent_skill_tool_test_{}_{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(SkillStore::new(dir))
}

fn out_text(out: &ToolOutput) -> String {
    match out {
        ToolOutput::Success(v) => v.to_string(),
        ToolOutput::Error(e) => e.clone(),
    }
}

fn is_err(out: &ToolOutput) -> bool {
    matches!(out, ToolOutput::Error(_))
}

#[test]
fn test_tool_roundtrip_save_list_read_delete() {
    let store = temp_store("rt");

    let save = SaveSkillTool::new(store.clone());
    let out = save
        .execute(tool_params(serde_json::json!({
            "name": "demo",
            "description": "A demo procedure",
            "when_to_use": "When testing",
            "body": "1. do this\n2. do that"
        })))
        .unwrap();
    assert!(!is_err(&out), "save failed: {}", out_text(&out));
    assert!(out_text(&out).contains("demo"));

    let list = ListSkillsTool::new(store.clone());
    let out = list.execute(tool_params(serde_json::json!({}))).unwrap();
    let text = out_text(&out);
    assert!(!is_err(&out));
    assert!(text.contains("demo"), "list must show the skill: {text}");
    assert!(text.contains("A demo procedure"));

    let read = ReadSkillTool::new(store.clone());
    let out = read.execute(tool_params(serde_json::json!({ "name": "demo" }))).unwrap();
    let text = out_text(&out);
    assert!(!is_err(&out));
    assert!(text.contains("1. do this"), "read must return the body: {text}");
    assert!(text.contains("when_to_use: When testing"));

    let del = DeleteSkillTool::new(store.clone());
    let out = del.execute(tool_params(serde_json::json!({ "name": "demo" }))).unwrap();
    assert!(!is_err(&out), "delete failed: {}", out_text(&out));

    let out = list.execute(tool_params(serde_json::json!({}))).unwrap();
    assert!(out_text(&out).contains("No skills saved yet"));
    let _ = std::fs::remove_dir_all(store.root());
}

#[test]
fn test_save_skill_invalid_name() {
    let store = temp_store("badname");
    let save = SaveSkillTool::new(store.clone());
    let out = save
        .execute(tool_params(serde_json::json!({
            "name": "bad name!",
            "description": "d",
            "when_to_use": "w",
            "body": "b"
        })))
        .unwrap();
    assert!(is_err(&out), "invalid name must be an error output");
    assert!(out_text(&out).contains("name"));
    let _ = std::fs::remove_dir_all(store.root());
}

#[test]
fn test_save_skill_missing_params() {
    let store = temp_store("missing");
    let save = SaveSkillTool::new(store.clone());
    let out = save.execute(tool_params(serde_json::json!({}))).unwrap();
    // Empty name → rejected by validation (the manager's schema gate would
    // also catch it before execution).
    assert!(is_err(&out), "missing params must error: {}", out_text(&out));
    let _ = std::fs::remove_dir_all(store.root());
}

#[test]
fn test_read_skill_missing() {
    let store = temp_store("readmiss");
    let read = ReadSkillTool::new(store.clone());
    let out = read
        .execute(tool_params(serde_json::json!({ "name": "nope" })))
        .unwrap();
    assert!(is_err(&out));
    assert!(out_text(&out).contains("not found"));
    let _ = std::fs::remove_dir_all(store.root());
}

#[test]
fn test_delete_skill_missing() {
    let store = temp_store("delmiss");
    let del = DeleteSkillTool::new(store.clone());
    let out = del
        .execute(tool_params(serde_json::json!({ "name": "nope" })))
        .unwrap();
    assert!(is_err(&out));
    assert!(out_text(&out).contains("not found"));
    let _ = std::fs::remove_dir_all(store.root());
}

#[test]
fn test_list_skills_empty() {
    let store = temp_store("empty");
    let list = ListSkillsTool::new(store.clone());
    let out = list.execute(tool_params(serde_json::json!({}))).unwrap();
    assert!(!is_err(&out));
    assert!(out_text(&out).contains("No skills saved yet"));
    let _ = std::fs::remove_dir_all(store.root());
}
