use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agents::types::{ControlRequest, HandBackRequest};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};

/// A tool that returns this sub-session to its parent session.
///
/// Per-execution tool: each `Agent` running in a sub-session (its session
/// meta carries a `parent_session_id`) with `hand_back_enabled` gets its own
/// instance (wired to the agent's shared control mailbox), injected in
/// `Agent::builder` exactly like the per-agent `shell`, `handoff`, and
/// `restart` tools. It is NOT registered in `register_builtins` because it
/// needs execution-specific state.
///
/// Calling it ends the current agent's turn: the tool queues a
/// [`ControlRequest::HandBack`] in the shared control mailbox,
/// `Agent::run_llm_loop` drains it before the next LLM round
/// (`RunOutcome::HandBack`), and
/// `Agent::execute` records a marker in the sub-session store, emits
/// [`crate::types::AppEvent::AgentHandBack`] (carrying both session ids) and
/// ends the turn. The UI then posts `task` into the parent session, where
/// the original agent resumes with its full history.
pub struct HandBackTool {
    /// Shared per-execution control mailbox drained by `Agent::run_llm_loop`.
    mailbox: Arc<Mutex<Vec<ControlRequest>>>,
}

impl HandBackTool {
    pub fn new(mailbox: Arc<Mutex<Vec<ControlRequest>>>) -> Self {
        Self { mailbox }
    }
}

impl Tool for HandBackTool {
    fn name(&self) -> &str {
        "hand_back"
    }

    fn description(&self) -> &str {
        "Return the session to the parent session (the one that handed off to \
         this sub-session). Use when the work here is done or belongs back with \
         the original agent/context. Your turn ends when you call it."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "hand_back".to_string(),
            description: "Return this sub-session to its parent session".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "task".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "What the parent session should do next with the returned work; include the key results and context the original agent needs".to_string(),
                            nullable: false,
                        },
                    );
                    map
                }),
                required: vec!["task".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let task: String = params
            .get("task")
            .ok_or_else(|| ToolError::InvalidParams("task is required".to_string()))?;
        let task = task.trim().to_string();
        if task.is_empty() {
            return Err(ToolError::InvalidParams(
                "task must not be empty".to_string(),
            ));
        }

        {
            let mut guard = self.mailbox.lock().unwrap();
            if guard.iter().any(|r| matches!(r, ControlRequest::HandBack(_))) {
                return Err(ToolError::Execution(
                    "A hand-back is already pending".to_string(),
                ));
            }
            guard.push(ControlRequest::HandBack(HandBackRequest { task }));
        }

        Ok(ToolOutput::Success(serde_json::json!({
            "status": "hand_back_queued",
            "note": "Your turn ends now; the session will be returned to the parent session."
        })))
    }
}

#[cfg(test)]
mod tests;
