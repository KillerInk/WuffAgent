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
