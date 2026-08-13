use serde::{Deserialize, Serialize};
use chrono::{Utc, DateTime};
use crate::types::Message;

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
}
