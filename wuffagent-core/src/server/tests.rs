//! Unit tests for the `server` module (see `super`).

use super::*;

#[test]
fn test_server_manager_creation() {
    let server = ServerManager::new(
        "llama-server",
        "test_model.gguf",
        18080,
        0,
        2048,
        4,
        ServerArgs::default(),
    );
    assert!(!server.is_running());
    assert_eq!(server.get_error(), None);
}

#[test]
fn test_server_manager_get_args() {
    let args = ServerArgs {
        parallel: 4,
        cache_reuse: 64,
        ..Default::default()
    };
    let server =
        ServerManager::new("llama-server", "m.gguf", 18081, 99, 16384, 8, args.clone());
    assert_eq!(server.get_args().parallel, 4);
    assert_eq!(server.get_args().cache_reuse, 64);
    assert_eq!(server.get_args(), &args);
}

#[test]
fn test_parse_progress() {
    assert_eq!(parse_progress("loading model ... 100%"), Some(100.0));
    assert_eq!(parse_progress("loading model ... 50%"), Some(50.0));
    assert_eq!(parse_progress("loading model ... 75.5%"), Some(75.5));
    assert_eq!(parse_progress("no percentage here"), None);
    assert_eq!(parse_progress(""), None);
    assert_eq!(parse_progress("100"), None);
}

// ── Server status monitor tests ──────────────────────────────────────────

#[test]
fn test_server_status_info_default() {
    let status = crate::types::ServerStatusInfo::default();
    assert!(!status.reachable);
    assert!(status.slots.is_empty());
    assert!(status.model.is_none());
    assert!(status.n_ctx.is_none());
}

#[test]
fn test_server_status_info_busy_slots() {
    let mut status = crate::types::ServerStatusInfo::default();
    status.slots = vec![
        crate::types::SlotInfo {
            id: 0,
            is_processing: true,
            n_ctx: 2048,
        },
        crate::types::SlotInfo {
            id: 1,
            is_processing: false,
            n_ctx: 2048,
        },
    ];
    let (busy, total) = status.busy_slots();
    assert_eq!(busy, 1);
    assert_eq!(total, 2);
}

#[test]
fn test_slot_info_deserialization() {
    // Sample /slots response from llama.cpp
    let json = r#"[
        {
            "id": 0,
            "state": "busy",
            "n_ctx": 2048
        }
    ]"#;
    let slots: Vec<crate::types::SlotInfo> = serde_json::from_str(json).unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].id, 0);
    assert_eq!(slots[0].n_ctx, 2048);
    // `state` is not a field in SlotInfo (we use is_processing)
    assert!(!slots[0].is_processing); // defaults to false
}

#[test]
fn test_slot_info_deserialization_missing_fields() {
    // Minimal /slots response (all fields optional via #[serde(default)])
    let json = r#"[
        {
            "id": 0
        }
    ]"#;
    let slots: Vec<crate::types::SlotInfo> = serde_json::from_str(json).unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].id, 0);
    assert!(!slots[0].is_processing);
    assert_eq!(slots[0].n_ctx, 0);
}
