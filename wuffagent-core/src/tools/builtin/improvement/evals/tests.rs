//! Tool tests for the eval tools (2a).

use super::*;
use crate::memory::evals::EvalStore;
use crate::tools::types::{ToolOutput, ToolParams};
use std::sync::Arc;

fn tp(json: serde_json::Value) -> ToolParams {
    let values = json
        .as_object()
        .expect("object")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    ToolParams { values }
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

fn temp_store(tag: &str) -> Arc<EvalStore> {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent_eval_tool_test_{}_{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(EvalStore::new(dir))
}

#[test]
fn save_list_delete_round_trip() {
    let store = temp_store("rt");
    let save = SaveEvalTool::new(store.clone());
    let out = save
        .execute(tp(serde_json::json!({
            "agent": "wuffagent",
            "id": "g1",
            "task": "read the fleet metrics",
            "expect": "the output includes the per-agent metrics",
            "max_tool_calls": 25
        })))
        .unwrap();
    assert!(!is_err(&out), "save: {}", out_text(&out));

    let list = ListEvalsTool::new(store.clone());
    let out = list.execute(tp(serde_json::json!({ "agent": "wuffagent" }))).unwrap();
    let text = out_text(&out);
    assert!(text.contains("g1"), "list must show the eval: {text}");
    assert!(text.contains("read the fleet metrics"));
    assert!(text.contains("max_tool_calls: 25"));

    let del = DeleteEvalTool::new(store.clone());
    let out = del
        .execute(tp(serde_json::json!({ "agent": "wuffagent", "id": "g1" })))
        .unwrap();
    assert!(!is_err(&out), "delete: {}", out_text(&out));

    let out = list.execute(tp(serde_json::json!({ "agent": "wuffagent" }))).unwrap();
    assert!(out_text(&out).contains("No evals saved"));
}

#[test]
fn save_missing_agent_errors() {
    let store = temp_store("noagent");
    let save = SaveEvalTool::new(store.clone());
    let out = save
        .execute(tp(serde_json::json!({
            "id": "g1",
            "task": "t",
            "expect": "e"
        })))
        .unwrap();
    assert!(is_err(&out), "missing agent must error: {}", out_text(&out));
    assert!(out_text(&out).to_lowercase().contains("agent"));
}

#[test]
fn save_upserts_by_id() {
    let store = temp_store("upsert");
    let save = SaveEvalTool::new(store.clone());
    for task in ["v1", "v2"] {
        let out = save
            .execute(tp(serde_json::json!({
                "agent": "coder",
                "id": "a",
                "task": task,
                "expect": "e"
            })))
            .unwrap();
        assert!(!is_err(&out), "save: {}", out_text(&out));
    }
    let list = ListEvalsTool::new(store.clone());
    let text = out_text(
        &list.execute(tp(serde_json::json!({ "agent": "coder" }))).unwrap(),
    );
    assert!(text.contains("v2"), "upsert must update in place: {text}");
    assert_eq!(
        store.list("coder").len(),
        1,
        "upsert must not duplicate: {text}"
    );
}

#[test]
fn delete_missing_eval_errors() {
    let store = temp_store("delmiss");
    let del = DeleteEvalTool::new(store.clone());
    let out = del
        .execute(tp(serde_json::json!({ "agent": "coder", "id": "nope" })))
        .unwrap();
    assert!(is_err(&out), "delete missing must error: {}", out_text(&out));
    assert!(out_text(&out).contains("not found"));
}

#[test]
fn list_empty_agent() {
    let store = temp_store("empty");
    let list = ListEvalsTool::new(store.clone());
    let out = list
        .execute(tp(serde_json::json!({ "agent": "coder" })))
        .unwrap();
    assert!(!is_err(&out));
    assert!(out_text(&out).contains("No evals saved"));
}
