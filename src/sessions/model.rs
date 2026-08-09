use serde::{Deserialize, Serialize};
use chrono::{Utc, DateTime};
use crate::types::Message;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    pub system_prompt: String,
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
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.touch();
    }
}
