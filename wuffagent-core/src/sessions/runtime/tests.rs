//! Unit tests for the `runtime` module (see `super`).

use super::*;

#[test]
fn thinking_chunks_feed_live_estimate() {
    let mut s = ChatAreaState::default();
    s.is_generating = true;
    // Thinking chunk: display buffer + live counters (TG must track it).
    s.stream_thinking_chunk("hello world"); // 11 chars
    assert_eq!(s.current_thinking, "hello world");
    assert_eq!(s.live_gen_chars, 11);
    assert!(s.live_gen_started.is_some());
    // Content chunk: same counters, different buffers.
    s.stream_chunk("abc"); // 3 chars
    assert_eq!(s.stream_buffer, "abc");
    assert_eq!(s.current_thinking, "hello world");
    assert_eq!(s.live_gen_chars, 14);
    assert!((s.live_gen_tokens() - 14.0 / 3.5).abs() < 1e-9);
}

#[test]
fn update_live_estimates_no_double_count() {
    let mut s = ChatAreaState::default();
    s.is_generating = true;
    s.token_count = 100;
    s.stream_chunk(&"x".repeat(35)); // ~10 estimated tokens
    s.update_live_estimates(1000);
    let after_first = s.token_count;
    assert!(after_first > 100, "live gauge must grow while generating");
    // Applying again WITHOUT new chunks must add nothing (the old code
    // re-added the whole cumulative segment every chunk — quadratic).
    s.update_live_estimates(1000);
    assert_eq!(s.token_count, after_first);
    // A new chunk adds only its own share.
    s.stream_chunk(&"y".repeat(35));
    s.update_live_estimates(1000);
    let after_second = s.token_count;
    assert!(after_second > after_first);
    assert!((after_second as f64 - after_first as f64 - 35.0 / 3.5).abs() < 1.0);
}

#[test]
fn update_live_estimates_noop_when_not_generating() {
    let mut s = ChatAreaState::default();
    s.token_count = 42;
    s.stream_chunk(&"x".repeat(35));
    s.update_live_estimates(1000);
    assert_eq!(s.token_count, 42);
    assert!(s.gen_tps.is_none());
    assert_eq!(s.context_used, 0.0);
}

#[test]
fn update_live_estimates_sets_context_used() {
    let mut s = ChatAreaState::default();
    s.is_generating = true;
    s.stream_chunk(&"x".repeat(350)); // 100 estimated tokens
    s.update_live_estimates(1000);
    assert!((s.context_used - (100.0 / 1000.0 * 100.0)).abs() < 1.0);
}

#[test]
fn commit_stream_resets_live_estimate() {
    let mut s = ChatAreaState::default();
    s.is_generating = true;
    s.token_count = 100;
    s.stream_chunk(&"x".repeat(35));
    s.update_live_estimates(1000);
    let grown = s.token_count;
    assert!(grown > 100);
    s.commit_stream();
    assert_eq!(s.live_gen_chars, 0);
    assert!(s.live_gen_started.is_none());
    assert_eq!(s.live_tokens_added, 0.0);
    // Next segment starts counting from a clean slate.
    s.stream_chunk(&"y".repeat(35));
    s.update_live_estimates(1000);
    assert!(s.token_count > grown);
}

#[test]
fn live_gen_tps_0_5s_gate() {
    let mut s = ChatAreaState::default();
    s.stream_chunk("some text");
    assert!(
        s.live_gen_tps().is_none(),
        "under 0.5s the estimate is meaningless"
    );
    std::thread::sleep(std::time::Duration::from_millis(550));
    let tps = s
        .live_gen_tps()
        .expect("after 0.5s the live speed is available");
    assert!(tps > 0.0);
}
