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

    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.touch();
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
