use serde::{Deserialize, Serialize};
use chrono::{Utc, DateTime};
use crate::types::Message;

/// Status of an agent session.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum SessionStatus {
    #[default]
    Active,
    Paused,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionStatus::Active => write!(f, "active"),
            SessionStatus::Paused => write!(f, "paused"),
        }
    }
}

/// Status of an agent chain entry.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentChainEntryStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for AgentChainEntryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentChainEntryStatus::Pending => write!(f, "pending"),
            AgentChainEntryStatus::Running => write!(f, "running"),
            AgentChainEntryStatus::Completed => write!(f, "completed"),
            AgentChainEntryStatus::Failed => write!(f, "failed"),
        }
    }
}

/// An entry in the agent execution chain, recording which agent handled a request.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentChainEntry {
    pub agent_name: String,
    pub request: String,
    pub result: String,
    pub depth: u32,
    pub tool_calls: Vec<String>,
    pub completed_at: DateTime<Utc>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub status: AgentChainEntryStatus,
    /// Message history snapshot for pause/resume (only present when paused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Vec<Message>>,
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
    pub agent_chain: Vec<AgentChainEntry>,
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
            agent_chain: Vec::new(),
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

    /// Truncate agent_chain to last N entries to prevent unbounded growth.
    pub fn truncate_agent_chain(&mut self, max_entries: usize) {
        if self.agent_chain.len() > max_entries {
            self.agent_chain.drain(..self.agent_chain.len() - max_entries);
        }
    }

    /// Mark this session as paused and save a message checkpoint.
    pub fn mark_paused(&mut self, checkpoint: Vec<Message>) {
        self.status = SessionStatus::Paused;
        // Store checkpoint in the last chain entry so it survives save/load.
        if let Some(entry) = self.agent_chain.last_mut() {
            entry.checkpoint = Some(checkpoint);
        }
    }

    /// Check if this session was paused (has a checkpoint available).
    pub fn has_checkpoint(&self) -> bool {
        self.agent_chain
            .iter()
            .rev()
            .find_map(|e| e.checkpoint.as_ref())
            .is_some()
    }

    /// Restore messages from the latest checkpoint and resume the session.
    pub fn resume_from_checkpoint(&mut self) {
        if let Some(checkpoint) = self
            .agent_chain
            .iter()
            .rev()
            .find_map(|e| e.checkpoint.clone())
        {
            self.messages = checkpoint;
            self.status = SessionStatus::Active;
            // Clear the checkpoint after resuming.
            if let Some(entry) = self.agent_chain.last_mut() {
                entry.checkpoint = None;
            }
        }
    }

    /// Get the checkpoint messages, if any.
    pub fn get_checkpoint(&self) -> Option<&Vec<Message>> {
        self.agent_chain
            .iter()
            .rev()
            .find_map(|e| e.checkpoint.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn empty_assistant() -> Message {
        Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// `add_message` must never store a system message — that is how system
    /// prompts leaked into the session history before the redesign.
    #[test]
    fn add_message_rejects_system_role() {
        let mut s = Session::new("t");
        s.add_message(msg("system", "You are a helpful assistant."));
        assert!(s.messages.is_empty(), "system message must not be stored");
        // Non-system messages still work.
        s.add_message(msg("user", "hi"));
        assert_eq!(s.messages.len(), 1);
    }

    /// sanitize() pulls stray system messages out of the history into the
    /// dedicated prompt field, drops empty assistant placeholders, and removes
    /// exact duplicates — repairing legacy corrupted files in place.
    #[test]
    fn sanitize_extracts_system_drops_placeholders_and_dups() {
        let mut s = Session::new("t");
        s.messages.push(msg("system", "You are a helpful assistant."));
        s.messages.push(msg("user", "hi"));
        s.messages.push(empty_assistant());
        s.messages.push(msg("assistant", "hello"));
        // Exact duplicate of the "assistant: hello" message.
        s.messages.push(msg("assistant", "hello"));

        s.sanitize();

        assert!(
            !s.messages.iter().any(|m| m.role == "system"),
            "no system message may remain in the history"
        );
        assert_eq!(s.system_prompt, "You are a helpful assistant.");
        assert!(
            !s.messages.iter().any(|m| {
                m.role == "assistant" && m.content.is_empty() && m.tool_calls.is_none()
            }),
            "empty assistant placeholders must be dropped"
        );
        // user + single assistant (duplicate collapsed).
        assert_eq!(s.messages.len(), 2);
    }

    /// sanitize() must not overwrite a prompt that is already set — the stored
    /// system_prompt field wins over whatever was found in the history.
    #[test]
    fn sanitize_keeps_existing_system_prompt() {
        let mut s = Session::new("t");
        s.system_prompt = "stored prompt".to_string();
        s.messages.push(msg("system", "legacy prompt"));
        s.messages.push(msg("user", "hi"));
        s.sanitize();
        assert_eq!(s.system_prompt, "stored prompt");
        assert!(s.messages.iter().all(|m| m.role != "system"));
    }

    /// A two-turn conversation, appended message-by-message the way the agent
    /// loop does, stays in order with no system message and no duplication.
    #[test]
    fn multi_turn_appends_once_in_order() {
        let mut s = Session::new("t");
        // Turn 1
        s.add_message(msg("user", "turn 1 user"));
        s.add_message(msg("assistant", "turn 1 assistant"));
        // Turn 2
        s.add_message(msg("user", "turn 2 user"));
        s.add_message(msg("assistant", "turn 2 assistant"));
        s.sanitize();

        let roles: Vec<_> = s.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        let contents: Vec<_> = s.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["turn 1 user", "turn 1 assistant", "turn 2 user", "turn 2 assistant"]);
    }
}
