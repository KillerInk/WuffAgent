use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A pending request to hand the session over to another agent profile.
///
/// Written by the `handoff` tool into the per-execution mailbox and consumed
/// by `Agent::execute`, which switches to the target agent on the same
/// conversation store.
#[derive(Clone, Debug)]
pub struct HandoffRequest {
    /// Target agent name.
    pub agent: String,
    /// Resolved target profile (system prompt, tools, shell, effort, …).
    pub config: super::config::AgentConfig,
    /// Handoff instructions for the target agent (used as its task / memory
    /// query and shown in the UI banner).
    pub task: String,
}

/// A pending request to restart the WuffAgent process (written by the
/// `restart` tool into the per-execution mailbox).
///
/// The agent loop picks it up after the tool round and returns
/// `RunOutcome::Restart`; `Agent::execute` emits an
/// [`crate::types::AppEvent::RestartRequested`], the UI relaunches the
/// (optionally newly built) binary and closes the window, and a marker file
/// lets the new process resume this session automatically.
#[derive(Clone, Debug)]
pub struct RestartRequest {
    /// Why the agent is restarting (shown in the UI and used to build the
    /// auto-resume turn).
    pub reason: String,
    /// The build command that was run before restarting (if any), for logging.
    pub build_cmd: Option<String>,
    /// Path to the binary the UI should launch (None = relaunch the current
    /// executable). On Windows this typically points into a `--target-dir` so
    /// the running exe is not relinked while the app is still up.
    pub exe_path: Option<String>,
}

/// I1: tool-use trajectory stats for one agent run, fed to the improver so
/// it can weigh HOW the agent worked (tool churn, errors, verification
/// retries), not just the final text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    /// Total tool calls executed this run (across all LLM rounds).
    pub tool_calls: usize,
    /// Tool calls that returned an error ("Error: ..." tool output).
    pub tool_errors: usize,
    /// Verification judge attempts used (0 = no tool outputs / shortcut).
    pub verification_attempts: u32,
}

/// Unique identifier for an agent instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(id: &str) -> Self {
        Self(id.to_string())
    }
    pub fn generate() -> Self {
        Self(format!("agent-{}", Uuid::new_v4()))
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests;
