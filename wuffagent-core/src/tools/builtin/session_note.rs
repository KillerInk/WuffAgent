//! `session_note` — pin a short state note to the current session (S4a).
//!
//! Per-execution tool (wired to the agent's shared control mailbox),
//! injected in `Agent::builder` like the per-agent `shell`, `handoff`,
//! `restart` and `hand_back` tools. Calling it does NOT end the turn: the
//! note is inserted as an anchored user message (right after the system
//! prompt) by `run_llm_loop` before the next LLM round and recorded in the
//! shared store, so it survives trims and session reloads (re-anchored on
//! every LLM call).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agents::types::{ControlRequest, SessionNoteRequest};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};

/// A tool that pins a short state note to the current session.
///
/// Per-execution tool: each `Agent` gets its own instance (wired to the
/// agent's shared control mailbox), injected in `Agent::builder` exactly like
/// the per-agent `shell`, `handoff`, `restart`, and `hand_back` tools. It is
/// NOT registered in `register_builtins` because it needs execution-specific
/// state.
///
/// Calling it does not end the current agent's turn: the tool queues a
/// [`ControlRequest::SessionNote`] in the shared control mailbox, and
/// `Agent::run_llm_loop` drains it before the next LLM round — inserting the
/// note as an anchored user message (right after the system prompt) via
/// `crate::trimming::brief::apply_note` and recording it in the shared store
/// so the note survives trims and session reloads.
pub struct SessionNoteTool {
    /// Shared per-execution control mailbox drained by `Agent::run_llm_loop`.
    mailbox: Arc<Mutex<Vec<ControlRequest>>>,
}

impl SessionNoteTool {
    pub fn new(mailbox: Arc<Mutex<Vec<ControlRequest>>>) -> Self {
        Self { mailbox }
    }
}

impl Tool for SessionNoteTool {
    fn name(&self) -> &str {
        "session_note"
    }

    fn description(&self) -> &str {
        "Pin a short state note (≤2000 chars; keep it one line) to this \
         context compaction and session reloads. Use it when you reach a \
         significant state that a future (possibly re-anchored) you must not \
         lose: a decision, an invariant, the next step, or where work stands. \
         The note is anchored right after the system prompt in every later \
         LLM call. Your turn CONTINUES after you call it (unlike handoff). \
         At most 3 notes are kept; the oldest is folded (capped at 400 chars) into the session \
         brief."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "session_note".to_string(),
            description: "Pin a short session state note".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "note".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "The state to pin (one line, ≤2000 chars): a decision, an invariant, the next step, or where work stands".to_string(),
                            nullable: false,
                        },
                    );
                    map
                }),
                required: vec!["note".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let note: String = params
            .get("note")
            .ok_or_else(|| ToolError::InvalidParams("note is required".to_string()))?;
        let note = note.trim().to_string();
        if note.is_empty() {
            return Err(ToolError::InvalidParams("note must not be empty".to_string()));
        }

        {
            let mut guard = self.mailbox.lock().unwrap();
            if guard.iter().any(|r| matches!(r, ControlRequest::SessionNote(_))) {
                return Err(ToolError::Execution(
                    "A session note is already pending for this round; it will be \
                     applied before the next LLM call (call it again after that)"
                        .to_string(),
                ));
            }
            guard.push(ControlRequest::SessionNote(SessionNoteRequest { note }));
        }

        Ok(ToolOutput::Success(serde_json::json!({
            "status": "session_note_queued",
            "note": "The note is pinned to the session and will be visible from the next LLM round. Continue your turn."
        })))
    }
}

#[cfg(test)]
mod tests;
