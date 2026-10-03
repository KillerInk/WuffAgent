//! Host-bridge command execution (P1): drains the plugin command queue once
//! per frame on the UI thread (called from `process_pending_events`).
//!
//! Everything here runs on the UI thread — fast, non-blocking operations
//! only (the plugin thread is blocked in `recv_timeout` while we work).

use super::state::ChatApp;
use crate::host_bridge::{HostCommand, HostReply};

impl ChatApp {
    /// Drain the plugin host command queue. Each command runs immediately and
    /// is answered on its one-shot reply channel; the plugin thread unblocks
    /// with the result (or timed out at 5 s — in which case the reply is
    /// dropped as the sender was).
    pub(super) fn process_host_commands(&mut self) {
        // Take the receiver out (it is the only handle) so the per-command
        // `&mut self` work below doesn't fight a borrow of `self.host`;
        // put it back at the end (same pattern as process_pending_events).
        let Some(rx) = self.host.rx.take() else {
            return;
        };
        loop {
            let Ok(cmd) = rx.try_recv() else {
                break;
            };
            match cmd {
                HostCommand::Inject { sid, text, reply } => {
                    let _ = reply.send(self.host_inject(&sid, &text));
                }
                HostCommand::Create { name, reply } => {
                    let _ = reply.send(self.host_create(&name));
                }
                HostCommand::Resolve { query, reply } => {
                    let _ = reply.send(self.host_resolve(&query));
                }
                HostCommand::Switch { sid, reply } => {
                    let _ = reply.send(self.host_switch(&sid));
                }
                HostCommand::Count { reply } => {
                    let _ = reply.send(HostReply::OkCount(self.sessions.session_store.len()));
                }
                HostCommand::Get { index, reply } => {
                    let _ = reply.send(self.host_get(index));
                }
            }
        }
        self.host.rx = Some(rx);
    }

    /// Inject a user message into `sid` (auto-creates the session first, per
    /// the ABI). Mirrors the UI's own send path: the session's agent
    /// selection (if any) drives the prompt and tool policy.
    fn host_inject(&mut self, sid: &str, text: &str) -> HostReply {
        if text.trim().is_empty() {
            return HostReply::Err;
        }
        if !self.sessions.session_store.contains_key(sid) {
            match self.host_create(sid) {
                HostReply::OkId(_) => {}
                other => return other,
            }
        }
        let Some(runtime) = self.sessions.session_store.get(sid) else {
            return HostReply::Err;
        };
        let agent = runtime.selected_agent.clone().unwrap_or_default();
        let agent_prompt = self.resolve_agent_prompt(&agent);
        let tool_policy = self.resolve_tool_policy(&agent);
        self.start_pipeline_for_session(sid, text, None, agent_prompt, tool_policy, false);
        HostReply::Ok
    }

    /// Create a named session — mirrors the sessions panel's `Create` action
    /// (defaults: "general" agent, top-level session; selects it in the
    /// sidebar and persists it as the active session).
    fn host_create(&mut self, name: &str) -> HostReply {
        let sessions_dir = match self.sessions_panel.as_ref() {
            Some(panel) => panel.sessions_dir().clone(),
            None => return HostReply::Err,
        };
        let pending_tx = match self.relay.pending_tx.as_ref() {
            Some(tx) => tx.lock().unwrap().clone(),
            None => return HostReply::Err,
        };
        let session = wuffagent_core::sessions::create_session(&sessions_dir, name);
        let mut runtime = wuffagent_core::sessions::SessionRuntime::create_from_config(
            &self.core.config,
            &self.core.connection,
            &self.core.agent_engine,
            session.id.clone(),
            session.name.clone(),
            pending_tx,
        );
        // Same defaults as the panel's Create: "general" agent, top-level.
        runtime.selected_agent = Some("general".to_string());
        runtime.client.set_session_meta(wuffagent_core::sessions::SessionMeta {
            selected_agent: runtime.selected_agent.clone(),
            reasoning_mode: runtime.reasoning_mode,
            parent_session_id: None,
        });
        self.sessions
            .session_store
            .insert(session.id.clone(), runtime);
        if let Some(panel) = self.sessions_panel.as_mut() {
            *panel.selected_id_mut() = Some(session.id.clone());
            {
                let mut cfg = panel.config().clone();
                cfg.session_id = Some(session.id.clone());
                if let Err(e) = cfg.save() {
                    tracing::warn!(
                        error = %e,
                        "host bridge: failed to save config after session create"
                    );
                }
            }
            panel.refresh();
        }
        HostReply::OkId(session.id)
    }

    /// Resolve a session by exact id, or by name (case-insensitive; first
    /// match in sorted-id order — deterministic).
    fn host_resolve(&mut self, query: &str) -> HostReply {
        if self.sessions.session_store.contains_key(query) {
            return HostReply::OkId(query.to_string());
        }
        let lower = query.to_lowercase();
        let mut matches: Vec<&String> = self
            .sessions
            .session_store
            .iter()
            .filter(|(_, rt)| rt.name.to_lowercase() == lower)
            .map(|(id, _)| id)
            .collect();
        matches.sort();
        match matches.into_iter().next() {
            Some(id) => HostReply::OkId(id.clone()),
            None => HostReply::Err,
        }
    }

    /// Switch the UI to `sid` (so the desktop follows the plugin's active
    /// session); `Err` when the session is unknown.
    fn host_switch(&mut self, sid: &str) -> HostReply {
        if !self.sessions.session_store.contains_key(sid) {
            return HostReply::Err;
        }
        self.switch_session(sid);
        HostReply::Ok
    }

    /// The (id, name) of the `index`-th session in sorted-id order
    /// (deterministic — a plugin may rely on it across calls).
    fn host_get(&mut self, index: usize) -> HostReply {
        let mut entries: Vec<(
            &String,
            &wuffagent_core::sessions::SessionRuntime,
        )> = self.sessions.session_store.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        match entries.get(index) {
            Some((id, rt)) => HostReply::OkSession {
                id: (**id).clone(),
                name: rt.name.clone(),
            },
            None => HostReply::Err,
        }
    }
}
