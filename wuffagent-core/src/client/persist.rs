//! Session persistence methods for ChatClient (thin orchestration over the
//! free functions in `crate::sessions::persist`). Split out of the client
//! facade (C1).

use super::ChatClient;
use crate::sessions::persist::{
    clear_save_failure, enqueue_save_failure, has_save_failure, load_session,
    retry_pending_saves, save_session,
};

impl ChatClient {
    pub fn load_session(&mut self) -> Option<crate::sessions::Session> {
        load_session(
            self.session_id.as_deref(),
            &self.session_dir,
            &self.conversation,
            &mut self.system_prompt,
            self.encryption_key.as_ref(),
        )
    }

    pub fn save_session(&self) -> Result<(), anyhow::Error> {
        save_session(
            self.session_id.as_deref(),
            &self.session_dir,
            &self.conversation,
            &self.system_prompt,
            &self.session_meta,
            self.encryption_key.as_ref(),
            &self.save_queue,
            &self.save_failed,
        )
    }

    /// Enqueue a pending save and set the failure flag for UI notification.
    pub fn enqueue_save_failure(&self, error: &anyhow::Error) {
        enqueue_save_failure(&self.save_queue, &self.save_failed, error);
    }

    /// Try to retry any pending saves and clear the queue on success.
    pub fn retry_pending_saves(&self) {
        let _ = retry_pending_saves(&self.save_queue, &self.save_failed, &|| {
            save_session(
                self.session_id.as_deref(),
                &self.session_dir,
                &self.conversation,
                &self.system_prompt,
                &self.session_meta,
                self.encryption_key.as_ref(),
                &self.save_queue,
                &self.save_failed,
            )
        });
    }

    /// Returns true if there is a pending save failure notification to show.
    pub fn has_save_failure(&self) -> bool {
        has_save_failure(&self.save_failed)
    }

    /// Clear the save failure flag (call after a successful save or user dismissal).
    pub fn clear_save_failure(&self) {
        clear_save_failure(&self.save_failed);
    }
}
