use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::sessions;
use crate::types::Message;

/// Session save/load/encrypt/decrypt orchestration for ChatClient.

/// Save the current conversation to the active session.
pub fn save_session(
    session_id: Option<&str>,
    session_dir: &PathBuf,
    conversation: &Arc<Mutex<Vec<Message>>>,
    encryption_key: Option<&[u8; 32]>,
    save_queue: &Arc<Mutex<VecDeque<()>>>,
    save_failed: &Arc<Mutex<bool>>,
) -> Result<(), anyhow::Error> {
    let id = session_id.ok_or_else(|| anyhow::anyhow!("no session id"))?;
    let conv = conversation.lock().unwrap();
    // Try loading the session; if it's encrypted, fall back to creating a new one
    // (the key will be used to re-encrypt on the next save).
    let mut session = if let Some(key) = encryption_key {
        sessions::decrypt_and_load_session(session_dir, id, key)
            .or_else(|| sessions::load_session(session_dir, id))
            .ok_or_else(|| anyhow::anyhow!("session not found"))?
    } else {
        sessions::load_session(session_dir, id)
            .ok_or_else(|| anyhow::anyhow!("session not found"))?
    };
    session.messages = conv.clone();
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
                std::thread::sleep(std::time::Duration::from_millis(50u64.pow(retries as u32)));
                eprintln!("Session save attempt {} failed: {}, retrying...", retries, e);
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
    session_dir: &PathBuf,
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
    let session = session?;
    let mut conv = conversation.lock().unwrap();
    *conv = session.messages.clone();
    if !session.system_prompt.is_empty() {
        *system_prompt = session.system_prompt.clone();
    }
    Some(session)
}

/// Enqueue a pending save and set the failure flag for UI notification.
pub fn enqueue_save_failure(
    save_queue: &Arc<Mutex<VecDeque<()>>>,
    save_failed: &Arc<Mutex<bool>>,
    error: &anyhow::Error,
) {
    let mut queue = save_queue.lock().unwrap();
    queue.push_back(());
    drop(queue);
    let mut flagged = save_failed.lock().unwrap();
    *flagged = true;
    eprintln!("Session save failed, enqueued for retry: {}", error);
}

/// Try to retry any pending saves and clear the queue on success.
/// Returns true if the retry succeeded, false otherwise.
pub fn retry_pending_saves(
    save_queue: &Arc<Mutex<VecDeque<()>>>,
    save_failed: &Arc<Mutex<bool>>,
    save_session_fn: &dyn Fn() -> Result<(), anyhow::Error>,
) -> bool {
    let mut queue = save_queue.lock().unwrap();
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
        let mut flagged = save_failed.lock().unwrap();
        *flagged = false;
        true
    }
}

/// Returns true if there is a pending save failure notification to show.
pub fn has_save_failure(save_failed: &Arc<Mutex<bool>>) -> bool {
    *save_failed.lock().unwrap()
}

/// Clear the save failure flag (call after a successful save or user dismissal).
pub fn clear_save_failure(save_failed: &Arc<Mutex<bool>>) {
    let mut flagged = save_failed.lock().unwrap();
    *flagged = false;
}

/// Clear the internal retry queue without attempting a save.
pub fn clear_save_queue(save_queue: &Arc<Mutex<VecDeque<()>>>, save_failed: &Arc<Mutex<bool>>) {
    let mut queue = save_queue.lock().unwrap();
    queue.clear();
    drop(queue);
    let mut flagged = save_failed.lock().unwrap();
    *flagged = false;
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
    let start = keep_from.min(trim_at);
    conv.drain(..start);
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
        eprintln!("Failed to save session after clear: {}", e);
    }
}
