//! Session save/load/retry orchestration over the disk-level persistence
//! functions in the parent module.
//!
//! Moved out of `client/session.rs` (Phase 2, E2a) so the client no longer
//! owns session persistence: this module is self-contained — it depends only
//! on the disk fns + `Session` in the parent `sessions` module and on pure
//! `std`/`anyhow` queue helpers. That relocates the persistence logic to the
//! module that owns the `Session` type, reducing the `client → sessions` edge
//! of the documented 3-cycle `client → sessions → agents → client` to a thin
//! black-box call from the client.
//!
//! Naming note: these orchestrators share the names `save_session`/`load_session`
//! with the *disk* fns in `sessions/mod.rs` (different signatures). They live in
//! this submodule so the two don't collide; the disk `load_session` is imported
//! here as `load_session_disk`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{
    decrypt_and_load_session, load_session as load_session_disk, save_session_atomic,
    save_session_encrypted, session_exists, Session, SessionMeta,
};
use crate::types::Message;

/// Save the current conversation to the active session.
///
/// `system_prompt` is persisted in the session's dedicated field — never in
/// `messages`. The stored history is sanitized before writing so legacy files
/// that accumulated stray system messages or duplicate blocks heal in place.
///
/// `meta` carries the per-session UI selections (chosen agent profile +
/// reasoning-effort mode); it is stamped onto the session so both survive
/// with the file (old files without the fields load as `None`/Auto).
pub fn save_session(
    session_id: Option<&str>,
    session_dir: &std::path::Path,
    conversation: &Arc<Mutex<Vec<Message>>>,
    system_prompt: &str,
    meta: &SessionMeta,
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
        if let Some(s) = decrypt_and_load_session(session_dir, id, key) {
            s
        } else if let Some(s) = load_session_disk(session_dir, id) {
            s
        } else if session_exists(session_dir, id) {
            // File exists but decryption failed — re-raise the error
            return Err(anyhow::anyhow!("session not found"));
        } else {
            tracing::warn!("Session file not found for id={}, creating new session", id);
            Session::new("Untitled")
        }
    } else {
        if let Some(s) = load_session_disk(session_dir, id) {
            s
        } else if session_exists(session_dir, id) {
            // File exists but parse failed — re-raise the error
            return Err(anyhow::anyhow!("session not found"));
        } else {
            tracing::warn!("Session file not found for id={}, creating new session", id);
            Session::new("Untitled")
        }
    };
    session.messages = conv.clone();
    session.sanitize();
    // Keep the persisted system prompt in sync with the in-memory one, but
    // prefer a prompt recovered from a legacy file (sanitize) when the
    // in-memory one is empty.
    if !system_prompt.is_empty() || session.system_prompt.is_empty() {
        session.system_prompt = system_prompt.to_string();
    }
    // Stamp the current per-session UI selections (chosen agent + reasoning
    // mode) so they persist with the session.
    session.selected_agent = meta.selected_agent.clone();
    session.reasoning_mode = meta.reasoning_mode;
    session.id = id.to_string();
    // Retry with exponential backoff for transient failures
    let mut retries = 0;
    loop {
        let save_result = if let Some(key) = encryption_key {
            save_session_encrypted(session_dir, &session, key)
        } else {
            save_session_atomic(session_dir, &session)
        };
        match save_result {
            Ok(()) => {
                // Success: clear any pending queue and failure flag
                clear_save_queue(save_queue);
                return Ok(());
            }
            Err(e) if retries < 3 => {
                retries += 1;
                // Cap backoff at 1s to avoid long freezes on persistent failures
                let backoff_ms = (50u64.pow(retries as u32)).min(1000);
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                tracing::error!(
                    "Session save attempt {} failed: {}, retrying...",
                    retries,
                    e
                );
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
) -> Option<Session> {
    let id = session_id?;
    let session = if let Some(key) = encryption_key {
        decrypt_and_load_session(session_dir, id, key)
    } else {
        load_session_disk(session_dir, id)
    };
    let mut session = session?;
    // Repair legacy files in place (extract stray system messages into the
    // session's prompt field, drop duplicates and empty placeholders).
    session.sanitize();
    let mut conv = conversation.lock().ok()?;
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
    save_failed.lock().map(|m| *m).unwrap_or(false)
}

/// Clear the save failure flag (call after a successful save or user dismissal).
pub fn clear_save_failure(save_failed: &Arc<Mutex<bool>>) {
    if let Ok(mut flagged) = save_failed.lock() {
        *flagged = false;
    }
}

/// Clear the internal retry queue without attempting a save.
pub fn clear_save_queue(save_queue: &Arc<Mutex<VecDeque<()>>>) {
    if let Ok(mut queue) = save_queue.lock() {
        queue.clear();
    }
}
