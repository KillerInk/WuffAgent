//! `SessionState`: the per-session runtime state shared between the
//! `ChatClient` and the session persistence layer (`sessions::persist`).
//!
//! A `ChatClient` carries one `SessionState`; cloning the client (agent runs
//! clone it per agent, e.g. for the per-agent reasoning effort) shares the
//! SAME state — previously only the conversation buffer was shared through
//! an `Arc`, while the identity fields (session id/dir, system prompt,
//! encryption key, save queue) were value-copied at clone time and could
//! drift between the original and its clones. Every field is
//! interior-mutable, so `&self` accessors can read and update it on a shared
//! client, and cloning the handle is a cheap pointer bump.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::model::SessionMeta;
use crate::types::Message;

/// The per-session state a [`crate::client::ChatClient`] carries: the
/// conversation buffer plus everything the save/load/retry orchestration
/// ([`super::persist`]) needs.
///
/// All fields are interior-mutable (`Arc<Mutex<..>>`) so `&self` accessors
/// can read or update them on a shared client; the whole struct is a cheaply
/// cloneable handle — every clone observes the same conversation and the
/// same session identity.
#[derive(Clone, Default)]
pub struct SessionState {
    session_id: Arc<Mutex<Option<String>>>,
    session_dir: Arc<Mutex<PathBuf>>,
    conversation: Arc<Mutex<Vec<Message>>>,
    system_prompt: Arc<Mutex<String>>,
    session_meta: Arc<Mutex<SessionMeta>>,
    encryption_key: Arc<Mutex<Option<[u8; 32]>>>,
    save_queue: Arc<Mutex<VecDeque<()>>>,
    save_failed: Arc<Mutex<bool>>,
    /// Serializes concurrent saves of this session: the off-thread turn-end
    /// save worker may run in parallel with a sync save / retry from the UI
    /// thread, and two saves writing the same `<id>.json.tmp` must not
    /// interleave (torn file).
    save_lock: Arc<Mutex<()>>,
}

impl SessionState {
    /// Create state for a specific session (used when a client is bound to
    /// a session, e.g. by `SessionRuntime::create_from_config`).
    pub fn new(session_id: Option<String>, session_dir: PathBuf) -> Self {
        Self {
            session_id: Arc::new(Mutex::new(session_id)),
            session_dir: Arc::new(Mutex::new(session_dir)),
            ..Default::default()
        }
    }

    /// The active session id (`None` = no session bound yet).
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().unwrap().clone()
    }

    pub fn set_session_id(&self, session_id: Option<String>) {
        *self.session_id.lock().unwrap() = session_id;
    }

    /// The directory session files are read from / written to.
    pub fn session_dir(&self) -> PathBuf {
        self.session_dir.lock().unwrap().clone()
    }

    pub fn set_session_dir(&self, session_dir: PathBuf) {
        *self.session_dir.lock().unwrap() = session_dir;
    }

    /// The shared in-memory conversation buffer. Returns a reference into
    /// this state (the buffer itself is already an `Arc<Mutex<..>>`), so
    /// callers can keep using `&Arc<Mutex<Vec<Message>>>` plumbing.
    pub fn conversation(&self) -> &Arc<Mutex<Vec<Message>>> {
        &self.conversation
    }

    /// The system prompt (owned clone — it lives behind a mutex).
    pub fn system_prompt(&self) -> String {
        self.system_prompt.lock().unwrap().clone()
    }

    pub fn set_system_prompt(&self, prompt: &str) {
        *self.system_prompt.lock().unwrap() = prompt.to_string();
    }

    /// The per-session UI selections persisted with the session file
    /// (owned clone — it lives behind a mutex).
    pub fn session_meta(&self) -> SessionMeta {
        self.session_meta.lock().unwrap().clone()
    }

    pub fn set_session_meta(&self, meta: SessionMeta) {
        *self.session_meta.lock().unwrap() = meta;
    }

    /// The session-file encryption key (`None` = unencrypted files).
    pub fn encryption_key(&self) -> Option<[u8; 32]> {
        *self.encryption_key.lock().unwrap()
    }

    pub fn set_encryption_key(&self, key: Option<[u8; 32]>) {
        *self.encryption_key.lock().unwrap() = key;
    }

    /// The pending-save retry queue (cleared on a successful save).
    pub fn save_queue(&self) -> &Arc<Mutex<VecDeque<()>>> {
        &self.save_queue
    }

    /// Set when a save failed and was enqueued for retry (UI notification).
    pub fn save_failed(&self) -> &Arc<Mutex<bool>> {
        &self.save_failed
    }

    /// Per-session save serialization lock (see the field docs).
    pub fn save_lock(&self) -> &Arc<Mutex<()>> {
        &self.save_lock
    }
}
