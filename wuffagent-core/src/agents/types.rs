use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A pending request to hand the session over to another agent profile.
///
/// Written by the `handoff` tool into the per-execution mailbox and consumed
/// by `Agent::execute`, which either switches to the target agent on the same
/// conversation store (in-turn chain) or — when `sub_session` is set — ends
/// this turn and lets the UI fork a clean sub-session for the target.
#[derive(Clone, Debug)]
pub struct HandoffRequest {
    /// Target agent name.
    pub agent: String,
    /// Resolved target profile (system prompt, tools, shell, effort, …).
    pub config: super::config::AgentConfig,
    /// Handoff instructions for the target agent (used as its task / memory
    /// query and shown in the UI banner).
    pub task: String,
    /// When true, the handoff forks a CLEAN sub-session instead of chaining
    /// in-turn on the same store: the target agent starts with only its own
    /// system prompt + this task, in a new session whose `parent_session_id`
    /// points back to this one (the `hand_back` link).
    pub sub_session: bool,
}

/// A pending request to hand the session back to its parent session (written
/// by the `hand_back` tool into the per-execution mailbox; only sub-sessions
/// — sessions whose session meta carries a `parent_session_id` — get the
/// tool).
///
/// The agent loop picks it up after the tool round and returns
/// `RunOutcome::HandBack`; `Agent::execute` records a marker in the
/// sub-session store, emits an [`crate::types::AppEvent::AgentHandBack`]
/// (carrying both session ids), and ends the turn. The UI then posts `task`
/// into the parent session, where the original agent resumes with its full
/// history.
#[derive(Clone, Debug)]
pub struct HandBackRequest {
    /// What the parent session should do next with the returned work (shown
    /// in the parent's turn and used as its task / memory query).
    pub task: String,
}

/// A pending request to pin a session note to the current session (S4a:
/// written by the `session_note` tool into the per-execution mailbox;
/// available on every agent run).
///
/// The agent loop picks it up before the next LLM round, inserts the note as
/// an anchored user message right after the system prompt (via
/// `trimming::brief::apply_note`) and records it in the shared store, so the
/// note survives trims and session reloads (it is re-anchored on every LLM
/// call). Unlike `HandoffRequest`/`HandBackRequest`/`RestartRequest` it does
/// NOT end the turn — the run continues with the note visible from the next
/// LLM round.
#[derive(Clone, Debug)]
pub struct SessionNoteRequest {
    /// The note text (the agent's own one-line state, ≤400 chars).
    pub note: String,
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

/// A control request queued by one of the per-execution tools (`handoff`,
/// `restart`, `hand_back`, `session_note`) into the shared control mailbox
/// (`Agent::drain_control`).
///
/// The four tools each push their own variant; `Agent::run_llm_loop` drains
/// the mailbox once per LLM round boundary and processes the variants in the
/// legacy priority order (notes first — they never end the round — then
/// handoff > restart > hand_back, whichever is pending ends the run). Each
/// tool rejects a second queued request of its own variant, so at most one
/// of each variant is pending at a time.
// `Handoff` carries a full `AgentConfig` while the other variants are small;
// at most four requests are ever queued per session, so the padding is
// immaterial.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ControlRequest {
    Handoff(HandoffRequest),
    Restart(RestartRequest),
    HandBack(HandBackRequest),
    SessionNote(SessionNoteRequest),
}

impl ControlRequest {
    /// The handoff request, if this is a handoff.
    pub fn as_handoff(&self) -> Option<&HandoffRequest> {
        match self {
            Self::Handoff(r) => Some(r),
            _ => None,
        }
    }

    /// The restart request, if this is a restart.
    pub fn as_restart(&self) -> Option<&RestartRequest> {
        match self {
            Self::Restart(r) => Some(r),
            _ => None,
        }
    }

    /// The hand-back request, if this is a hand-back.
    pub fn as_hand_back(&self) -> Option<&HandBackRequest> {
        match self {
            Self::HandBack(r) => Some(r),
            _ => None,
        }
    }

    /// The session-note request, if this is a session note.
    pub fn as_session_note(&self) -> Option<&SessionNoteRequest> {
        match self {
            Self::SessionNote(r) => Some(r),
            _ => None,
        }
    }
}

/// 1a: per-tool aggregate for one run (the per-tool breakdown of the scalar
/// `RunStats::tool_calls`/`tool_errors` counters, plus per-tool duration —
/// the evidence for "which tools are slow or error-prone").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStat {
    /// Tool name; `"__other__"` is the fold bucket for runs that used more
    /// than 32 distinct tools (keeps the run line small).
    pub name: String,
    /// Calls recorded for this tool (including truncation errors, which
    /// never execute).
    pub calls: u32,
    /// Calls that returned an error ("Error: ..." tool output).
    pub errors: u32,
    /// Cumulative wall-clock milliseconds spent in this tool.
    pub duration_ms: u64,
}

/// I1: tool-use trajectory stats for one agent run, fed to the improver so
/// it can weigh HOW the agent worked (tool churn, errors, verification
/// retries), not just the final text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunStats {
    /// Total tool calls executed this run (across all LLM rounds).
    pub tool_calls: usize,
    /// Tool calls that returned an error ("Error: ..." tool output).
    pub tool_errors: usize,
    /// Verification judge attempts used (0 = no tool outputs / shortcut).
    pub verification_attempts: u32,
    /// 1a: per-tool breakdown (folded, ≤33 entries).
    pub tools: Vec<ToolStat>,
    /// 1d: cumulative wall-clock ms spent in LLM calls — the main-round
    /// streaming calls, plus the verification judge's time (folded in by
    /// the loop before the run line is written, so `llm_ms` means "all
    /// time spent in the LLM").
    pub llm_ms: u64,
}

impl RunStats {
    /// 1a: fold one tool call into the histogram AND the scalar counters
    /// (the scalars stay the source of truth for summaries; `tools` is the
    /// breakdown of the same events).
    ///
    /// Hot path (every tool call of every run): O(n) scan over ≤33 entries,
    /// no allocation except first-seen tool names.
    pub fn bump_tool(&mut self, name: &str, error: bool, ms: u64) {
        self.tool_calls += 1;
        if error {
            self.tool_errors += 1;
        }
        const FOLD: &str = "__other__";
        let target = if self.tools.iter().any(|t| t.name == name) || self.tools.len() < 32 {
            name
        } else {
            FOLD
        };
        match self.tools.iter_mut().find(|t| t.name == target) {
            Some(t) => {
                t.calls += 1;
                if error {
                    t.errors += 1;
                }
                t.duration_ms += ms;
            }
            None => self.tools.push(ToolStat {
                name: target.to_string(),
                calls: 1,
                errors: u32::from(error),
                duration_ms: ms,
            }),
        }
    }
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
