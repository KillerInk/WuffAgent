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

#[tokio::test]
async fn attach_mode_when_port_busy() {
    // Occupy a random port; starting the server against it must ATTACH
    // (running + attached, no spawn attempt, fake path never executed)
    // instead of failing with a port collision.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = ServerManager::new(
        "llama-server-does-not-exist",
        "m.gguf",
        port as u16,
        0,
        4096,
        4,
        ServerArgs::default(),
    );
    assert!(!server.is_attached());

    // Async start path: probes the port, sees it busy, attaches.
    assert!(server
        .start_server_with_paths(
            "llama-server-does-not-exist",
            "m.gguf",
            port as u16,
            0,
            4096,
            4
        )
        .await
        .is_ok());
    assert!(server.is_running());
    assert!(server.is_attached());

    // stop_server on an attached manager clears the flags without killing
    // anything (no child of ours).
    assert!(server.stop_server().await.is_ok());
    assert!(!server.is_running());
    assert!(server.is_attached());
    drop(listener);
}

#[test]
fn attach_mode_when_port_free() {
    // No listener on a random free port → attach_if_running is a no-op.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // port is now free

    let server = ServerManager::new("llama-server", "m.gguf", port as u16, 0, 4096, 4, ServerArgs::default());
    assert!(!server.attach_if_running(port as u16));
    assert!(!server.is_running());
    assert!(!server.is_attached());
}

// (parse_progress tests live in server/progress/tests.rs — next to the
// function; a duplicate copy used to live here.)

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
    // Sample /slots response from llama.cpp (b11126 server-slot to_json:
    // `id`, `n_ctx`, `speculative`, `is_processing` — the extra fields are
    // ignored by the subset struct).
    let json = r#"[
        {
            "id": 0,
            "state": "busy",
            "n_ctx": 2048,
            "speculative": false,
            "is_processing": true
        }
    ]"#;
    let slots: Vec<crate::types::SlotInfo> = serde_json::from_str(json).unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].id, 0);
    assert_eq!(slots[0].n_ctx, 2048);
    assert!(slots[0].is_processing);
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

// ── /metrics Prometheus text parsing (Phase 2 item 6) ─────────────────────

#[test]
fn test_parse_metrics_text_full() {
    // Sample of the b11126 /metrics output (llamacpp: prefix, # HELP/# TYPE
    // comment lines, labeled speculative series, unknown names).
    let text = "\
# HELP llamacpp:prompt_tokens_total Number of prompt tokens processed, excluding cached tokens
# TYPE llamacpp:prompt_tokens_total counter
llamacpp:prompt_tokens_total 12345
# HELP llamacpp:prompt_tokens_seconds Average prompt throughput in tokens/s
# TYPE llamacpp:prompt_tokens_seconds gauge
llamacpp:prompt_tokens_seconds 432.109
# HELP llamacpp:predicted_tokens_seconds Average generation throughput in tokens/s
# TYPE llamacpp:predicted_tokens_seconds gauge
llamacpp:predicted_tokens_seconds 18.734
# HELP llamacpp:requests_processing Number of requests processing
# TYPE llamacpp:requests_processing gauge
llamacpp:requests_processing 2
# HELP llamacpp:n_tokens_max Largest observed sequence length (prompt + generation)
# TYPE llamacpp:n_tokens_max counter
llamacpp:n_tokens_max 65535
llamacpp:spec_decode_num_accepted_tokens_per_pos_total{position=\"0\"} 10
";
    let m = status::parse_metrics_text(text).unwrap();
    assert_eq!(m.prompt_tps, Some(432.109));
    assert_eq!(m.predicted_tps, Some(18.734));
    assert_eq!(m.requests_processing, Some(2));
    assert_eq!(m.n_tokens_max, Some(65535));
}

#[test]
fn test_parse_metrics_text_empty_and_unknown() {
    // No recognizable llamacpp: series → None.
    assert!(status::parse_metrics_text("").is_none());
    assert!(status::parse_metrics_text("# only comments\n# here\n").is_none());
    assert!(status::parse_metrics_text(
        "some_other_metric 42\nllamacpp:unknown_name 1.0"
    )
    .is_none());
}

#[test]
fn test_parse_metrics_text_partial() {
    // Only some of the fields present — the rest stay None.
    let m = status::parse_metrics_text("llamacpp:requests_processing 1\n").unwrap();
    assert_eq!(m.requests_processing, Some(1));
    assert_eq!(m.prompt_tps, None);
    assert_eq!(m.predicted_tps, None);
    assert_eq!(m.n_tokens_max, None);
}

#[test]
fn test_parse_metrics_text_bad_values() {
    // Malformed values are skipped, good ones kept.
    let m = status::parse_metrics_text(
        "llamacpp:predicted_tokens_seconds not_a_number\nllamacpp:prompt_tokens_seconds 9.5",
    )
    .unwrap();
    assert_eq!(m.predicted_tps, None);
    assert_eq!(m.prompt_tps, Some(9.5));
}

// ── /v1/models → n_ctx_train extraction (Phase 2 item 6) ──────────────────

#[test]
fn test_n_ctx_train_extraction_from_shapes() {
    // b11126 shape: {"data": [{"meta": {"n_ctx_train": N}}]}.
    let json = r#"{"data": [{"id": "m.gguf", "meta": {"n_ctx_train": 32768, "n_vocab": 128256}}], "object": "list"}"#;
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(status::n_ctx_train_from_models(&value), Some(32768));

    // Bare-array shape + top-level n_ctx_train fallback.
    let json = r#"[{"n_ctx_train": 4096}]"#;
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(status::n_ctx_train_from_models(&value), Some(4096));

    // Missing field / empty list / empty data → None.
    let value: serde_json::Value = serde_json::from_str(r#"{"data": []}"#).unwrap();
    assert_eq!(status::n_ctx_train_from_models(&value), None);
    let value: serde_json::Value = serde_json::from_str(r#"{"data": [{"id": "x"}]}"#).unwrap();
    assert_eq!(status::n_ctx_train_from_models(&value), None);
    let value: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(status::n_ctx_train_from_models(&value), None);
}
