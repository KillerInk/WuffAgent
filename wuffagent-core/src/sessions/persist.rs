//! Session save/load/retry orchestration over the disk-level persistence
//! functions in the parent module.
//!
//! Moved out of `client/session.rs` (Phase 2, E2a) so the client no longer
//! owns session persistence. Everything operates on a
//! [`super::state::SessionState`] (the shared per-session state a
//! `ChatClient` carries) instead of a soup of eight separate arguments —
//! that is what makes the client's `save_session`/`load_session` methods a
//! one-line delegation.
//!
//! Naming note: these orchestrators share the names `save_session`/`load_session`
//! with the *disk* fns in `sessions/mod.rs` (different signatures). They live in
//! this submodule so the two don't collide; the disk `load_session` is imported
//! here as `load_session_disk`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::state::SessionState;
use super::{
    decrypt_and_load_session, load_session as load_session_disk, save_session_atomic,
    save_session_encrypted, session_exists, Session,
};

/// Save the current conversation to the active session.
///
/// `state.system_prompt()` is persisted in the session's dedicated field —
/// never in `messages`. The stored history is sanitized before writing so
/// legacy files that accumulated stray system messages or duplicate blocks
/// heal in place.
///
/// The per-session UI selections (chosen agent profile + reasoning-effort
/// mode, in `state.session_meta()`) are stamped onto the session so both
/// survive with the file (old files without the fields load as `None`/Auto).
pub fn save_session(state: &SessionState) -> Result<(), anyhow::Error> {
    let id = state.session_id().ok_or_else(|| anyhow::anyhow!("no session id"))?;
    let session_dir = state.session_dir();
    let conv = state.conversation();
    let system_prompt = state.system_prompt();
    let meta = state.session_meta();
    let encryption_key = state.encryption_key();
    let save_queue = state.save_queue();

    let conv = conv.lock().unwrap();
    // Try loading the session; if it's encrypted, fall back to creating a new one
    // (the key will be used to re-encrypt on the next save).
    // If the file doesn't exist at all, create a new session so saves work.
    let key_slice = encryption_key.as_ref().map(|k| k.as_slice());
    let mut session = if let Some(key) = key_slice {
        // Try decrypting first, then fall back to a plain load
        if let Some(s) = decrypt_and_load_session(&session_dir, &id, key) {
            s
        } else if let Some(s) = load_session_disk(&session_dir, &id) {
            s
        } else if session_exists(&session_dir, &id) {
            // File exists but decryption failed — re-raise the error
            return Err(anyhow::anyhow!("session not found"));
        } else {
            tracing::warn!("Session file not found for id={}, creating new session", id);
            Session::new("Untitled")
        }
    } else if let Some(s) = load_session_disk(&session_dir, &id) {
        s
    } else if session_exists(&session_dir, &id) {
        // File exists but parse failed — re-raise the error
        return Err(anyhow::anyhow!("session not found"));
    } else {
        tracing::warn!("Session file not found for id={}, creating new session", id);
        Session::new("Untitled")
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
    // mode) so they persist with the session, plus the sub-session parent
    // link (None for top-level sessions).
    session.selected_agent = meta.selected_agent.clone();
    session.reasoning_mode = meta.reasoning_mode;
    session.parent_session_id = meta.parent_session_id.clone();
    session.id = id;
    // Retry with exponential backoff for transient failures
    let mut retries = 0;
    loop {
        let save_result = match encryption_key {
            Some(ref key) => save_session_encrypted(&session_dir, &session, key),
            None => save_session_atomic(&session_dir, &session),
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
                enqueue_save_failure(state, &e);
                return Err(e);
            }
        }
    }
}

/// Load a session from disk into the conversation (in `state`).
/// Returns the loaded Session if found, or None.
pub fn load_session(state: &SessionState) -> Option<Session> {
    let id = state.session_id()?;
    let session_dir = state.session_dir();
    let encryption_key = state.encryption_key();
    let session = match encryption_key {
        Some(ref key) => decrypt_and_load_session(&session_dir, &id, key),
        None => load_session_disk(&session_dir, &id),
    };
    let mut session = session?;
    // Repair legacy files in place (extract stray system messages into the
    // session's prompt field, drop duplicates and empty placeholders).
    session.sanitize();
    let mut conv = state.conversation().lock().ok()?;
    *conv = session.messages.clone();
    if !session.system_prompt.is_empty() {
        state.set_system_prompt(&session.system_prompt);
    }
    Some(session)
}

/// Enqueue a pending save and set the failure flag for UI notification.
pub fn enqueue_save_failure(state: &SessionState, error: &anyhow::Error) {
    if let Ok(mut queue) = state.save_queue().lock() {
        queue.push_back(());
    }
    if let Ok(mut flagged) = state.save_failed().lock() {
        *flagged = true;
    }
    tracing::error!("Session save failed, enqueued for retry: {}", error);
}

/// Try to retry any pending saves and clear the queue on success.
/// Returns true if the retry succeeded, false otherwise.
pub fn retry_pending_saves(
    state: &SessionState,
    save_session_fn: &dyn Fn() -> Result<(), anyhow::Error>,
) -> bool {
    let mut queue = match state.save_queue().lock() {
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
        enqueue_save_failure(state, &e);
        false
    } else {
        if let Ok(mut flagged) = state.save_failed().lock() {
            *flagged = false;
        }
        true
    }
}

/// Returns true if there is a pending save failure notification to show.
pub fn has_save_failure(state: &SessionState) -> bool {
    state.save_failed().lock().map(|m| *m).unwrap_or(false)
}

/// Clear the save failure flag (call after a successful save or user dismissal).
pub fn clear_save_failure(state: &SessionState) {
    if let Ok(mut flagged) = state.save_failed().lock() {
        *flagged = false;
    }
}

/// Clear the internal retry queue without attempting a save.
pub fn clear_save_queue(save_queue: &Arc<Mutex<VecDeque<()>>>) {
    if let Ok(mut queue) = save_queue.lock() {
        queue.clear();
    }
}
