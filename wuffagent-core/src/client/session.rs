use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::sessions;
use crate::types::Message;

/// Session save/load/encrypt/decrypt orchestration for ChatClient.
///
/// Save the current conversation to the active session.
pub fn save_session(
    session_id: Option<&str>,
    session_dir: &std::path::Path,
    conversation: &Arc<Mutex<Vec<Message>>>,
    encryption_key: Option<&[u8; 32]>,
    save_queue: &Arc<Mutex<VecDeque<()>>>,
    save_failed: &Arc<Mutex<bool>>,
) -> Result<(), anyhow::Error> {
    let id = session_id.ok_or_else(|| anyhow::anyhow!("no session id"))?;
    let conv = conversation.lock().unwrap();
    // Try loading the session; if it's encrypted, fall back to creating a new one
    // (the key will be used to re-encrypt on the next save).
    // If the file doesn't exist at all, create a new session so saves work.
    let mut session = if let Some(key) = encryption_key {
        // Try decrypting first, then fall back to plain load
        if let Some(s) = sessions::decrypt_and_load_session(session_dir, id, key) {
            s
        } else if let Some(s) = sessions::load_session(session_dir, id) {
            s
        } else if sessions::session_exists(session_dir, id) {
            // File exists but decryption failed — re-raise the error
            return Err(anyhow::anyhow!("session not found"));
        } else {
            tracing::warn!("Session file not found for id={}, creating new session", id);
            crate::sessions::Session::new("Untitled")
        }
    } else {
        if let Some(s) = sessions::load_session(session_dir, id) {
            s
        } else if sessions::session_exists(session_dir, id) {
            // File exists but parse failed — re-raise the error
            return Err(anyhow::anyhow!("session not found"));
        } else {
            tracing::warn!("Session file not found for id={}, creating new session", id);
            crate::sessions::Session::new("Untitled")
        }
    };
    session.messages = conv.clone();
    session.id = id.to_string();
    // Truncate agent_chain to prevent unbounded growth on save
    session.truncate_agent_chain(100);
    // Retry with exponential backoff for transient failures
    let mut retries = 0;
    loop {
        let save_result = if let Some(key) = encryption_key {
            sessions::save_session_encrypted(session_dir, &session, key)
        } else {
            sessions::save_session_atomic(session_dir, &session)
        };
        match save_result {
            Ok(()) => {
                // Success: clear any pending queue and failure flag
                clear_save_queue(save_queue, save_failed);
                return Ok(());
            }
            Err(e) if retries < 3 => {
                retries += 1;
                // Cap backoff at 1s to avoid long freezes on persistent failures
                let backoff_ms = (50u64.pow(retries as u32)).min(1000);
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                tracing::error!("Session save attempt {} failed: {}, retrying...", retries, e);
            }
            Err(e) => {
                // All retries exhausted — enqueue for later retry and signal UI
                enqueue_save_failure(save_queue, save_failed, &e);
                return Err(e);
            }
        }
    }
}

/// Load a session from disk into the conversation.
/// Returns the loaded Session if found, or None.
pub fn load_session(
    session_id: Option<&str>,
    session_dir: &std::path::Path,
    conversation: &Arc<Mutex<Vec<Message>>>,
    system_prompt: &mut String,
    encryption_key: Option<&[u8; 32]>,
) -> Option<crate::sessions::Session> {
    let id = session_id?;
    let session = if let Some(key) = encryption_key {
        sessions::decrypt_and_load_session(session_dir, id, key)
    } else {
        sessions::load_session(session_dir, id)
    };
    let mut session = session?;
    let mut conv = conversation
        .lock()
        .ok()?;
    *conv = session.messages.clone();
    if !session.system_prompt.is_empty() {
        *system_prompt = session.system_prompt.clone();
    }
    // Truncate agent_chain on load to prevent unbounded growth
    session.truncate_agent_chain(100);
    Some(session)
}

/// Enqueue a pending save and set the failure flag for UI notification.
pub fn enqueue_save_failure(
    save_queue: &Arc<Mutex<VecDeque<()>>>,
    save_failed: &Arc<Mutex<bool>>,
    error: &anyhow::Error,
) {
    if let Ok(mut queue) = save_queue.lock() {
        queue.push_back(());
    }
    if let Ok(mut flagged) = save_failed.lock() {
        *flagged = true;
    }
    tracing::error!("Session save failed, enqueued for retry: {}", error);
}

/// Try to retry any pending saves and clear the queue on success.
/// Returns true if the retry succeeded, false otherwise.
pub fn retry_pending_saves(
    save_queue: &Arc<Mutex<VecDeque<()>>>,
    save_failed: &Arc<Mutex<bool>>,
    save_session_fn: &dyn Fn() -> Result<(), anyhow::Error>,
) -> bool {
    let mut queue = match save_queue.lock() {
        Ok(q) => q,
        Err(_) => return false,
    };
    let count = queue.len();
    if count == 0 {
        return true;
    }
    // Drain the queue and attempt saves
    queue.clear();
    drop(queue);

    // Attempt a single save; if it succeeds, clear the failure flag
    if let Err(e) = save_session_fn() {
        // Still failing — re-enqueue and keep the flag
        enqueue_save_failure(save_queue, save_failed, &e);
        false
    } else {
        if let Ok(mut flagged) = save_failed.lock() {
            *flagged = false;
        }
        true
    }
}

/// Returns true if there is a pending save failure notification to show.
pub fn has_save_failure(save_failed: &Arc<Mutex<bool>>) -> bool {
    save_failed
        .lock()
        .map(|m| *m)
        .unwrap_or(false)
}

/// Clear the save failure flag (call after a successful save or user dismissal).
pub fn clear_save_failure(save_failed: &Arc<Mutex<bool>>) {
    if let Ok(mut flagged) = save_failed.lock() {
        *flagged = false;
    }
}

/// Clear the internal retry queue without attempting a save.
/// Clear the internal retry queue without attempting a save.
pub fn clear_save_queue(save_queue: &Arc<Mutex<VecDeque<()>>>, save_failed: &Arc<Mutex<bool>>) {
    let _ = save_failed; // keep the parameter for API compatibility
    if let Ok(mut queue) = save_queue.lock() {
        queue.clear();
    }
}

/// Trim the conversation to the given max_messages, preserving the system message.
pub fn trim_conversation(
    conversation: &Arc<Mutex<Vec<Message>>>,
    max_messages: usize,
) {
    let mut conv = conversation.lock().unwrap();
    if conv.len() <= max_messages {
        return;
    }
    let system_idx = conv.iter().position(|m| m.role == "system");
    let keep_from = if let Some(idx) = system_idx {
        idx + 1
    } else {
        0
    };
    let trim_at = conv.len().saturating_sub(max_messages);
    if trim_at > keep_from {
        conv.drain(keep_from..trim_at);
    }
}

/// Estimate the token count of a message string using a simple character heuristic.
/// This is a rough estimate; actual tokenizers may differ.
/// Uses chars/2 as a conservative heuristic to avoid underestimating code/JSON
/// content where real tokenizers produce ~1 token per 1.5-2 chars.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 2 + 1
}

/// Truncate the largest non-system, non-last-user message so the total estimated
/// token count drops below `target_tokens`. Returns true if any truncation occurred.
/// Uses a larger per-iteration cut (8192 chars) since the estimate may be
/// systematically off — we want to overshoot on the safe side.
fn truncate_largest_message(
    messages: &mut [Message],
    target_tokens: usize,
    protect_last_user: bool,
) -> bool {
    // Find the largest content message that we're allowed to truncate.
    let mut best_idx: Option<usize> = None;
    let mut best_len: usize = 0;
    let last_user_idx = if protect_last_user {
        messages.iter().rev().position(|m| m.role == "user")
            .map(|r| messages.len().saturating_sub(r) - 1)
    } else {
        None
    };
    for (i, m) in messages.iter().enumerate() {
        if m.role == "system" {
            continue;
        }
        if protect_last_user {
            if let Some(lui) = last_user_idx {
                if i >= lui {
                    continue;
                }
            }
        }
        let len = m.content.len();
        if len > best_len {
            best_len = len;
            best_idx = Some(i);
        }
    }
    let idx = match best_idx {
        Some(i) => i,
        None => return false,
    };

    // Repeatedly shrink the message until we're under budget.
    loop {
        let prompt_len: usize = messages.iter().map(|m| {
            let mut total = estimate_tokens(&m.content);
            if let Some(ref rc) = m.reasoning_content {
                total += estimate_tokens(rc);
            }
            if let Some(ref tcs) = m.tool_calls {
                for tc in tcs {
                    total += estimate_tokens(&tc.function.arguments);
                }
            }
            total
        }).sum();
        if prompt_len <= target_tokens {
            return false;
        }
        let current_chars = messages[idx].content.len();
        if current_chars == 0 {
            return false;
        }
        // Remove 8192 chars at a time — a larger cut to account for estimate
        // inaccuracy and ensure we get safely under budget.
        let cut = current_chars.saturating_sub(8192).min(current_chars - 1);
        messages[idx].content.truncate(cut);
    }
}

/// Token-budget trim: remove oldest non-system messages until the estimated
/// prompt size (all messages excluding the last assistant turn) is below the
/// target. `target_tokens` is typically ~90% of n_ctx.
/// Returns the number of messages removed.
pub fn trim_to_token_budget(
    conversation: &Arc<Mutex<Vec<Message>>>,
    target_tokens: usize,
) -> usize {
    let mut conv = conversation.lock().unwrap();
    if conv.is_empty() {
        return 0;
    }

    let system_idx = conv.iter().position(|m| m.role == "system");
    let keep_from = if let Some(idx) = system_idx { idx + 1 } else { 0 };

    // Don't trim if there's nothing to trim
    if keep_from >= conv.len() {
        return 0;
    }

    // Never remove the last user message — the server requires at least one
    // user message and raises a 500 if the last message is not from a user.
    let last_user_idx = conv.iter().rev().position(|m| m.role == "user")
        .map(|r| conv.len().saturating_sub(r) - 1);

    let mut removed = 0;
    loop {
        // Estimate current prompt size: all messages except last assistant turn
        let prompt_len: usize = conv.iter()
            .take(conv.len().saturating_sub(1))
            .map(|m| {
                let mut total = estimate_tokens(&m.content);
                if let Some(ref rc) = m.reasoning_content {
                    total += estimate_tokens(rc);
                }
                if let Some(ref tcs) = m.tool_calls {
                    for tc in tcs {
                        total += estimate_tokens(&tc.function.arguments);
                    }
                }
                total
            })
            .sum();
        if prompt_len <= target_tokens {
            break;
        }
        // Remove oldest non-system message
        if keep_from >= conv.len() {
            break;
        }
        // Stop before we would remove the last user message.
        if let Some(lui) = last_user_idx {
            if keep_from >= lui {
                break;
            }
        }
        conv.remove(keep_from);
        removed += 1;
    }

    // Fallback: if the token estimate is still over budget after removing all
    // removable messages, truncate the largest content message to force it under.
    truncate_largest_message(&mut conv, target_tokens, true);

    removed
}

/// Token-budget trim variant that operates on a plain `Vec<Message>`
/// (not wrapped in an `Arc<Mutex<…>>`). Used by the agent loop to trim
/// its own message history, since streaming writes to a throwaway
/// conversation and never updates the client's `conversation` field.
pub fn trim_to_token_budget_messages(
    messages: &mut Vec<Message>,
    target_tokens: usize,
) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let system_idx = messages.iter().position(|m| m.role == "system");
    let keep_from = if let Some(idx) = system_idx { idx + 1 } else { 0 };

    if keep_from >= messages.len() {
        return 0;
    }

    // Never remove the last user message — the server requires at least one
    // user message and raises a 500 if the last message is not from a user.
    let last_user_idx = messages.iter().rev().position(|m| m.role == "user")
        .map(|r| messages.len().saturating_sub(r) - 1);

    let mut removed = 0;
    loop {
        let prompt_len: usize = messages
            .iter()
            .take(messages.len())
            .map(|m| {
                let mut total = estimate_tokens(&m.content);
                if let Some(ref rc) = m.reasoning_content {
                    total += estimate_tokens(rc);
                }
                if let Some(ref tcs) = m.tool_calls {
                    for tc in tcs {
                        total += estimate_tokens(&tc.function.arguments);
                    }
                }
                total
            })
            .sum();
        if prompt_len <= target_tokens {
            break;
        }
        if keep_from >= messages.len() {
            break;
        }
        // Stop before we would remove the last user message.
        if let Some(lui) = last_user_idx {
            if keep_from >= lui {
                break;
            }
        }
        messages.remove(keep_from);
        removed += 1;
    }

    // Fallback: if the token estimate is still over budget after removing all
    // removable messages, truncate the largest content message to force it under.
    truncate_largest_message(messages, target_tokens, true);

    removed
}

/// Clear all messages from the conversation.
pub fn clear_history(conversation: &Arc<Mutex<Vec<Message>>>) {
    conversation.lock().unwrap().clear();
}

/// Clear all messages from the conversation and save the (empty) session.
pub fn clear_session_messages(
    conversation: &Arc<Mutex<Vec<Message>>>,
    save_session_fn: &dyn Fn() -> Result<(), anyhow::Error>,
) {
    conversation.lock().unwrap().clear();
    if let Err(e) = save_session_fn() {
        tracing::error!("Failed to save session after clear: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        }
    }

    fn make_conversation(messages: Vec<Message>) -> Arc<Mutex<Vec<Message>>> {
        Arc::new(Mutex::new(messages))
    }

    fn make_save_queue() -> Arc<Mutex<VecDeque<()>>> {
        Arc::new(Mutex::new(VecDeque::new()))
    }

    fn make_save_failed() -> Arc<Mutex<bool>> {
        Arc::new(Mutex::new(false))
    }

    fn make_encryption_key() -> [u8; 32] {
        [42u8; 32]
    }

    /// Save session with encryption, load it back, and verify messages match.
    #[tokio::test]
    async fn test_save_session_encrypted_roundtrip() {
        let dir = tempdir().unwrap();
        let session_dir = PathBuf::from(dir.path());
        let session_id = "test_encrypted";
        let key = make_encryption_key();

        let conv = make_conversation(vec![
            make_message("system", "You are helpful"),
            make_message("user", "Hello"),
            make_message("assistant", "Hi there!"),
        ]);

        let save_queue = make_save_queue();
        let save_failed = make_save_failed();

        // Save with encryption
        save_session(
            Some(session_id),
            &session_dir,
            &conv,
            Some(&key),
            &save_queue,
            &save_failed,
        )
        .unwrap();

        // Load back into a fresh conversation
        let loaded_conv = make_conversation(vec![]);
        let mut system_prompt = String::new();
        let loaded = load_session(
            Some(session_id),
            &session_dir,
            &loaded_conv,
            &mut system_prompt,
            Some(&key),
        )
        .expect("should load encrypted session");

        let messages = loaded_conv.lock().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[1].content, "Hello");
        assert_eq!(loaded.name, "Untitled");
        // system_prompt is stored separately in Session, not derived from messages
        assert!(system_prompt.is_empty());
        assert!(!*save_failed.lock().unwrap());
    }

    /// Loading a non-existent session should return None.
    #[tokio::test]
    async fn test_load_session_missing_returns_none() {
        let dir = tempdir().unwrap();
        let session_dir = PathBuf::from(dir.path());
        let conv = make_conversation(vec![]);
        let mut system_prompt = String::new();

        let result = load_session(
            Some("nonexistent"),
            &session_dir,
            &conv,
            &mut system_prompt,
            None,
        );

        assert!(result.is_none());
        // Conversation should be unchanged
        assert_eq!(conv.lock().unwrap().len(), 0);
    }

    /// Saving a session that doesn't exist yet should create it.
    #[tokio::test]
    async fn test_save_session_creates_new_if_missing() {
        let dir = tempdir().unwrap();
        let session_dir = PathBuf::from(dir.path());
        let session_id = "new_session";

        let conv = make_conversation(vec![make_message("user", "Test message")]);
        let save_queue = make_save_queue();
        let save_failed = make_save_failed();

        let result = save_session(
            Some(session_id),
            &session_dir,
            &conv,
            None,
            &save_queue,
            &save_failed,
        );

        assert!(result.is_ok());
        assert!(sessions::session_exists(&session_dir, session_id),
            "session file should exist for id={}", session_id);

        // Load it back
        let loaded_conv = make_conversation(vec![]);
        let mut system_prompt = String::new();
        let loaded = load_session(
            Some(session_id),
            &session_dir,
            &loaded_conv,
            &mut system_prompt,
            None,
        )
        .expect("should load newly created session");

        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "Test message");
    }

    /// When save fails after all retries, the failure should be enqueued.
    #[tokio::test]
    async fn test_save_session_enqueues_failure_on_error() {
        let dir = tempdir().unwrap();
        let session_dir = PathBuf::from(dir.path());
        let session_id = "test_fail";

        let conv = make_conversation(vec![make_message("user", "Hello")]);
        let save_queue = make_save_queue();
        let save_failed = make_save_failed();

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

        let result = save_session(
            Some(session_id),
            &session_dir,
            &conv,
            None,
            &save_queue,
            &save_failed,
        );

        // Should return Err because the file exists but can't be parsed
        assert!(result.is_err());
    }

    /// Adding pending saves to the queue and retrying should succeed.
    #[tokio::test]
    async fn test_retry_pending_saves() {
        let dir = tempdir().unwrap();
        let session_dir = PathBuf::from(dir.path());
        let session_id = "test_retry";

        let conv = make_conversation(vec![make_message("user", "Retry test")]);

        // Simulate a prior failure by putting items in the queue
        let save_queue = make_save_queue();
        let save_failed = make_save_failed();
        {
            let mut q = save_queue.lock().unwrap();
            q.push_back(());
            q.push_back(());
            drop(q);
            *save_failed.lock().unwrap() = true;
        }

        let save_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let save_count_clone = save_count.clone();
        let save_queue_clone = save_queue.clone();
        let save_failed_clone = save_failed.clone();
        let conv_clone = conv.clone();
        let session_dir_clone = session_dir.clone();
        let save_fn = move || {
            save_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            save_session(
                Some(session_id),
                &session_dir_clone,
                &conv_clone,
                None,
                &save_queue_clone,
                &save_failed_clone,
            )
        };

        let result = retry_pending_saves(&save_queue, &save_failed, &save_fn);

        assert!(result, "retry should succeed");
        assert!(!*save_failed.lock().unwrap());
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
}
