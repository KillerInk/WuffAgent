//! Unit tests for the eval store (2a): round-trip, upsert, corrupt line,
//! empty dir, delete, field validation, and the process-global test override.

use super::*;
use std::fs;
use std::io::Write;
use tempfile::tempdir;

fn store() -> (EvalStore, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    (EvalStore::new(dir.path().to_path_buf()), dir)
}

fn eval(id: &str, task: &str) -> Eval {
    Eval {
        id: id.to_string(),
        task: task.to_string(),
        expect: "verifies the expected outcome".to_string(),
        max_tool_calls: None,
    }
}

#[test]
fn round_trip() {
    let (s, _dir) = store();
    s.save(
        "coder",
        Eval {
            id: "self-restart".to_string(),
            task: "restart WuffAgent and verify the live build".to_string(),
            expect: "the running process path is the freshly built exe".to_string(),
            max_tool_calls: Some(50),
        },
    )
    .unwrap();
    let evals = s.list("coder");
    assert_eq!(evals.len(), 1);
    assert_eq!(evals[0].id, "self-restart");
    assert_eq!(evals[0].task, "restart WuffAgent and verify the live build");
    assert_eq!(evals[0].max_tool_calls, Some(50));
}

#[test]
fn upsert_by_id_does_not_duplicate() {
    let (s, _dir) = store();
    s.save("coder", eval("a", "v1")).unwrap();
    s.save("coder", eval("a", "v2")).unwrap();
    let evals = s.list("coder");
    assert_eq!(evals.len(), 1, "upsert must not duplicate: {evals:?}");
    assert_eq!(evals[0].task, "v2");
}

#[test]
fn corrupt_line_is_skipped() {
    let (s, dir) = store();
    s.save("coder", eval("ok", "t")).unwrap();
    // Append a corrupt line directly (bypassing save).
    let path = dir.path().join("coder.jsonl");
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"{ this is not valid json \n").unwrap();
    let evals = s.list("coder");
    assert_eq!(evals.len(), 1, "corrupt line must be skipped: {evals:?}");
    assert_eq!(evals[0].id, "ok");
}

#[test]
fn empty_dir_has_no_evals() {
    let (s, _dir) = store();
    assert!(s.list("coder").is_empty());
}

#[test]
fn delete_existing_and_missing() {
    let (s, _dir) = store();
    s.save("coder", eval("a", "t")).unwrap();
    s.save("coder", eval("b", "t")).unwrap();
    assert!(s.delete("coder", "a").unwrap());
    assert!(!s.delete("coder", "a").unwrap(), "second delete is a no-op");
    let evals = s.list("coder");
    assert_eq!(evals.len(), 1);
    assert_eq!(evals[0].id, "b");
}

#[test]
fn save_rejects_empty_fields() {
    let (s, _dir) = store();
    let e = s
        .save("coder", eval("   ", "t"))
        .expect_err("blank id must be rejected");
    assert!(e.contains("id"), "{e}");
    // Nothing was persisted.
    assert!(s.list("coder").is_empty());
}

#[test]
fn test_override_round_trip() {
    // The only test that touches the process-global default store.
    let dir = tempdir().unwrap();
    set_evals_dir_for_testing(Some(dir.path().to_path_buf()));
    EvalStore::default()
        .save("wuffagent", eval("g1", "t"))
        .unwrap();
    let back = EvalStore::default().list("wuffagent");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, "g1");
    set_evals_dir_for_testing(None);
}
