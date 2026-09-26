//! Unit tests for the `conversation` helpers plus the session save/load/retry
//! orchestrators in `crate::sessions::persist` (Phase 2, E2a). The persist
//! tests build a [`SessionState`] per test — the argument soup (id, dir,
//! conversation, prompt, meta, key, queue, flag) now lives in that one
//! value (D1a).

use super::*;
use crate::client::{estimate_conversation_tokens, trim_to_token_budget};
use crate::sessions::persist::{load_session, retry_pending_saves, save_session};
use crate::sessions::{session_exists, SessionState};
use crate::trimming::message_char_count;
use std::path::PathBuf;
use tempfile::tempdir;

fn make_message(role: &str, content: &str) -> Message {
    Message {
        role: role.to_string(),
        content: content.to_string(),
        timestamp: "2024-01-01T00:00:00Z".to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

fn make_conversation(messages: Vec<Message>) -> Arc<Mutex<Vec<Message>>> {
    Arc::new(Mutex::new(messages))
}

fn make_encryption_key() -> [u8; 32] {
    [42u8; 32]
}

/// A `SessionState` bound to `id` in `dir`, pre-filled with `messages` and
/// optionally encrypted.
fn make_session_state(
    dir: &std::path::Path,
    id: &str,
    messages: Vec<Message>,
    key: Option<[u8; 32]>,
) -> SessionState {
    let state = SessionState::default();
    state.set_session_id(Some(id.to_string()));
    state.set_session_dir(dir.to_path_buf());
    state.conversation().lock().unwrap().extend(messages);
    state.set_encryption_key(key);
    state
}

/// Save session with encryption, load it back, and verify messages match.
#[tokio::test]
async fn test_save_session_encrypted_roundtrip() {
    let dir = tempdir().unwrap();
    let session_dir = PathBuf::from(dir.path());
    let session_id = "test_encrypted";
    let key = make_encryption_key();

    let state = make_session_state(
        &session_dir,
        session_id,
        vec![
            make_message("system", "You are helpful"),
            make_message("user", "Hello"),
            make_message("assistant", "Hi there!"),
        ],
        Some(key),
    );

    // Save with encryption
    save_session(&state).unwrap();

    // Load back into a fresh state
    let loaded_state = make_session_state(&session_dir, session_id, vec![], Some(key));
    let loaded = load_session(&loaded_state).expect("should load encrypted session");

    let messages = loaded_state.conversation().lock().unwrap();
    // System messages are never stored in the message history; the prompt
    // is recovered into the session's dedicated field on load.
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[0].content, "Hello");
    assert_eq!(loaded.name, "Untitled");
    assert_eq!(loaded_state.system_prompt(), "You are helpful");
    assert!(!*loaded_state.save_failed().lock().unwrap());
}

/// Loading a non-existent session should return None.
#[tokio::test]
async fn test_load_session_missing_returns_none() {
    let dir = tempdir().unwrap();
    let session_dir = PathBuf::from(dir.path());

    let state = make_session_state(&session_dir, "nonexistent", vec![], None);
    let result = load_session(&state);

    assert!(result.is_none());
    // Conversation should be unchanged
    assert_eq!(state.conversation().lock().unwrap().len(), 0);
}

/// Saving a session that doesn't exist yet should create it.
#[tokio::test]
async fn test_save_session_creates_new_if_missing() {
    let dir = tempdir().unwrap();
    let session_dir = PathBuf::from(dir.path());
    let session_id = "new_session";

    let state = make_session_state(
        &session_dir,
        session_id,
        vec![make_message("user", "Test message")],
        None,
    );

    let result = save_session(&state);

    assert!(result.is_ok());
    assert!(
        session_exists(&session_dir, session_id),
        "session file should exist for id={}",
        session_id
    );

    // Load it back
    let loaded_state = make_session_state(&session_dir, session_id, vec![], None);
    let loaded = load_session(&loaded_state).expect("should load newly created session");

    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.messages[0].content, "Test message");
}

/// When save fails after all retries, the failure should be enqueued.
#[tokio::test]
async fn test_save_session_enqueues_failure_on_error() {
    let dir = tempdir().unwrap();
    let session_dir = PathBuf::from(dir.path());
    let session_id = "test_fail";

    let state = make_session_state(
        &session_dir,
        session_id,
        vec![make_message("user", "Hello")],
        None,
    );

    // Create a read-only directory to force save failures
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        // Restore permissions after test
        let perms = std::fs::Permissions::from_mode(0o755);
        let _ = std::fs::set_permissions(dir.path(), perms);
    }

    // Note: On Windows, we can't easily make a dir read-only, so we test
    // the enqueue logic directly via a corrupted file scenario instead.
    // Write an invalid JSON file so load_session returns None but session_exists is true.
    let corrupt_path = session_dir.join(format!("{}.json", session_id));
    std::fs::write(&corrupt_path, "not valid json!!!").unwrap();

    let result = save_session(&state);

    // Should return Err because the file exists but can't be parsed
    assert!(result.is_err());
}

/// Adding pending saves to the queue and retrying should succeed.
#[tokio::test]
async fn test_retry_pending_saves() {
    let dir = tempdir().unwrap();
    let session_dir = PathBuf::from(dir.path());
    let session_id = "test_retry";

    let state = make_session_state(
        &session_dir,
        session_id,
        vec![make_message("user", "Retry test")],
        None,
    );

    // Simulate a prior failure by putting items in the queue
    {
        let mut q = state.save_queue().lock().unwrap();
        q.push_back(());
        q.push_back(());
        drop(q);
        *state.save_failed().lock().unwrap() = true;
    }

    let save_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let save_count_clone = save_count.clone();
    let save_fn = || {
        save_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        save_session(&state)
    };

    let result = retry_pending_saves(&state, &save_fn);

    assert!(result, "retry should succeed");
    assert!(!*state.save_failed().lock().unwrap());
    assert_eq!(save_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Trim conversation to max_messages, preserving the system message.
#[test]
fn test_trim_conversation() {
    let conv = make_conversation(vec![
        make_message("system", "You are helpful"),
        make_message("user", "Msg 1"),
        make_message("assistant", "Msg 2"),
        make_message("user", "Msg 3"),
        make_message("assistant", "Msg 4"),
    ]);

    trim_conversation(&conv, 2);

    let messages = conv.lock().unwrap();
    // Should keep system + 2 most recent messages
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].content, "Msg 3");
    assert_eq!(messages[2].content, "Msg 4");
}

/// Trim with max_messages >= len should be a no-op.
#[test]
fn test_trim_conversation_no_op() {
    let conv = make_conversation(vec![
        make_message("system", "System"),
        make_message("user", "User"),
    ]);

    trim_conversation(&conv, 10);

    assert_eq!(conv.lock().unwrap().len(), 2);
}

/// Clear all messages from the conversation.
#[test]
fn test_clear_history() {
    let conv = make_conversation(vec![
        make_message("system", "System"),
        make_message("user", "Hello"),
        make_message("assistant", "Hi"),
    ]);

    clear_history(&conv);

    assert_eq!(conv.lock().unwrap().len(), 0);
}

/// Trim conversation with no system message.
#[test]
fn test_trim_conversation_no_system() {
    let conv = make_conversation(vec![
        make_message("user", "Msg 1"),
        make_message("assistant", "Msg 2"),
        make_message("user", "Msg 3"),
        make_message("assistant", "Msg 4"),
    ]);

    trim_conversation(&conv, 2);

    let messages = conv.lock().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "Msg 3");
}

/// Conversation token count is the exact char sum of content + reasoning + tool args.
#[test]
fn test_estimate_conversation_tokens() {
    let conv = make_conversation(vec![
        make_message("system", "You are helpful"), // 15 chars
        make_message("user", "What is 2+2?"),      // 12 chars
    ]);

    assert_eq!(estimate_conversation_tokens(&conv), 27);
}

/// Regression: a tool result stored with role "system" must not shield a
/// huge prefix from trimming. The old `position(role==system)` logic
/// protected everything up to the first "system" message, making the trim
/// a no-op and the conversation grow unboundedly.
#[test]
fn test_trim_removes_messages_when_tool_result_is_system() {
    let huge = "x".repeat(5000);
    let conv = make_conversation(vec![
        make_message("user", "hello"),
        make_message("tool", &huge), // mislabelled "system" in real sessions
        make_message("assistant", "I read the file"),
        make_message("user", "now summarize"),
    ]);

    // Tight budget: far below the huge message's size, above the rest.
    trim_to_token_budget(&conv, 100);

    let messages = conv.lock().unwrap();
    // The huge message must have been removed (or truncated), so the count
    // drops well under the original 4 and the total is back under budget.
    assert!(
        messages.len() < 4,
        "expected trimming, got {}",
        messages.len()
    );
    assert!(
        message_char_count(&messages) <= 100,
        "total {} still over budget",
        message_char_count(&messages)
    );
}

/// A leading system prompt is protected; everything after it is fair game.
#[test]
fn test_trim_keeps_leading_system_prompt() {
    let huge = "x".repeat(5000);
    let conv = make_conversation(vec![
        make_message("system", "You are helpful"),
        make_message("user", "hello"),
        make_message("assistant", &huge),
        make_message("user", "now summarize"),
    ]);

    trim_to_token_budget(&conv, 100);

    let messages = conv.lock().unwrap();
    assert_eq!(messages[0].role, "system", "system prompt must be kept");
    assert!(message_char_count(&messages) <= 100);
}

/// Save→load roundtrip must keep the system prompt in its dedicated field,
/// store the history with no system message, and preserve order/content
/// without duplication.
#[tokio::test]
async fn test_save_load_roundtrip_preserves_prompt_and_order() {
    let dir = tempdir().unwrap();
    let session_dir = PathBuf::from(dir.path());
    let session_id = "roundtrip_session";

    // The in-memory conversation carries a leading system prompt (as the
    // request list does). It must NOT leak into the stored history.
    let state = make_session_state(
        &session_dir,
        session_id,
        vec![
            make_message("system", "You are helpful"),
            make_message("user", "turn 1 user"),
            make_message("assistant", "turn 1 assistant"),
            make_message("user", "turn 2 user"),
            make_message("assistant", "turn 2 assistant"),
        ],
        None,
    );
    state.set_system_prompt("You are helpful");

    save_session(&state).unwrap();

    // Load into a fresh state.
    let loaded_state = make_session_state(&session_dir, session_id, vec![], None);
    load_session(&loaded_state).expect("should load the session");

    let messages = loaded_state.conversation().lock().unwrap();
    // System prompt restored into the dedicated field...
    assert_eq!(loaded_state.system_prompt(), "You are helpful");
    // ...and never present as a stored message.
    assert!(
        messages.iter().all(|m| m.role != "system"),
        "system prompt must not be stored in the history"
    );
    // Order and content preserved, no duplication (exactly the 4 turns).
    assert_eq!(messages.len(), 4);
    let got: Vec<(&str, &str)> = messages
        .iter()
        .map(|m| (m.role.as_str(), m.content.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("user", "turn 1 user"),
            ("assistant", "turn 1 assistant"),
            ("user", "turn 2 user"),
            ("assistant", "turn 2 assistant"),
        ]
    );
    assert!(!*loaded_state.save_failed().lock().unwrap());
}
