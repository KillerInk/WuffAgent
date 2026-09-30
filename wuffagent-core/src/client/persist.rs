//! Session persistence methods for ChatClient (thin delegation to the
//! free functions in `crate::sessions::persist`, which operate on the
//! client's shared [`crate::sessions::SessionState`]). Split out of the
//! client facade (C1); the argument soup moved into `SessionState` (D1a).

use super::ChatClient;
use crate::sessions::persist::{
    clear_save_failure, enqueue_save_failure, has_save_failure, load_session,
    retry_pending_saves, save_session,
};

impl ChatClient {
    pub fn load_session(&self) -> Option<crate::sessions::Session> {
        load_session(&self.session)
    }

    pub fn save_session(&self) -> Result<(), anyhow::Error> {
        save_session(&self.session)
    }

    /// Save the session on a detached worker thread (off-thread turn-end
    /// saves, perf-optimizations P6).
    ///
    /// `SessionState` is a cheaply cloneable `Arc<Mutex<..>>` handle, so the
    /// whole save — conversation snapshot, pretty-JSON serialize, atomic file
    /// write, retry with backoff — runs off the calling thread and a
    /// multi-MB session never hitches the UI exactly when a turn completes.
    /// Failures are logged here and still surface through the shared
    /// `save_failed` flag (UI notification + `retry_pending_saves`). The
    /// per-session save lock inside `save_session` serializes this with any
    /// concurrent sync save of the same session.
    pub fn save_session_async(&self) {
        let state = self.session.clone();
        let spawn = std::thread::Builder::new()
            .name("session-save".into())
            .spawn(move || {
                if let Err(e) = save_session(&state) {
                    tracing::warn!("off-thread session save failed: {e}");
                }
            });
        if spawn.is_err() {
            // Thread creation failed (OS thread limit): save synchronously
            // rather than silently losing the turn.
            let _ = save_session(&self.session);
        }
    }

    /// Enqueue a pending save and set the failure flag for UI notification.
    pub fn enqueue_save_failure(&self, error: &anyhow::Error) {
        enqueue_save_failure(&self.session, error);
    }

    /// Try to retry any pending saves and clear the queue on success.
    pub fn retry_pending_saves(&self) {
        let _ = retry_pending_saves(&self.session, &|| save_session(&self.session));
    }

    /// Returns true if there is a pending save failure notification to show.
    pub fn has_save_failure(&self) -> bool {
        has_save_failure(&self.session)
    }

    /// Clear the save failure flag (call after a successful save or user dismissal).
    pub fn clear_save_failure(&self) {
        clear_save_failure(&self.session);
    }
}
