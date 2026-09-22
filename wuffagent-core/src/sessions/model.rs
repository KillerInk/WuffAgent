use crate::types::Message;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of an agent session.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum SessionStatus {
    #[default]
    Active,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionStatus::Active => write!(f, "active"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    pub system_prompt: String,
    #[serde(default)]
    pub status: SessionStatus,
}

impl Session {
    pub fn new(name: &str) -> Self {
        let now = Utc::now();
        Self {
            id: format!("session_{}", now.timestamp_millis()),
            name: name.to_string(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            system_prompt: String::new(),
            status: SessionStatus::Active,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Add a message to this session.
    ///
    /// Invariant: system prompts are never stored as messages — they belong in
    /// [`Session::system_prompt`]. Any `system` role is rejected so corrupted
    /// history cannot be reintroduced through this API.
    pub fn add_message(&mut self, msg: Message) {
        if msg.role == "system" {
            tracing::warn!(
                "add_message: refusing to store a system message in session {} (system prompts live in Session.system_prompt)",
                self.id
            );
            return;
        }
        self.messages.push(msg);
        self.touch();
    }

    /// Repair and normalize the message history in place.
    ///
    /// Called on every session load so legacy/corrupted files heal in place at
    /// the first save. It enforces the storage invariants:
    /// 1. No `system` messages in `messages` — the first system message's
    ///    content is moved into [`Session::system_prompt`] (only when that
    ///    field is still empty) and all system messages are dropped.
    /// 2. Empty assistant messages without tool calls are dropped (leftover
    ///    streaming placeholders).
    /// 3. Exact duplicate messages (same role, content, tool_call_id and
    ///    tool_calls) are collapsed to their first occurrence — the previous
    ///    writer re-appended whole history blocks, so this repairs those files.
    pub fn sanitize(&mut self) {
        // 1. Extract system prompts out of the stored history.
        let mut extracted: Option<String> = None;
        self.messages.retain(|m| {
            if m.role != "system" {
                return true;
            }
            if extracted.is_none() && !m.content.is_empty() {
                extracted = Some(m.content.clone());
            }
            false
        });
        if self.system_prompt.is_empty() {
            if let Some(prompt) = extracted {
                self.system_prompt = prompt;
            }
        }

        // 2 + 3. Drop empty assistant placeholders and exact duplicates.
        let mut kept: Vec<Message> = Vec::with_capacity(self.messages.len());
        let mut seen = std::collections::HashSet::new();
        for m in std::mem::take(&mut self.messages) {
            if m.role == "assistant" && m.content.is_empty() && m.tool_calls.is_none() {
                continue;
            }
            // Build the dedup key from owned values so we can still move `m`
            // into `kept` below (the references into `m` cannot outlive it).
            let key = (
                m.role.clone(),
                m.content.clone(),
                m.tool_call_id.clone().unwrap_or_default(),
                serde_json::to_string(&m.tool_calls).unwrap_or_default(),
            );
            if !seen.insert(key) {
                continue;
            }
            kept.push(m);
        }
        self.messages = kept;
    }
}

#[cfg(test)]
mod tests;
