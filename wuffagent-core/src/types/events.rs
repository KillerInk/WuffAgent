//! Events that flow from the client engine to the UI.

use super::{ImprovementSuggestion, PromptProgress, QueuedMessage, Usage};

/// Events that flow from the client engine to the UI.
///
/// Each event carries a `session_id` so the UI can route it to the correct
/// session's chat area. When multiple sessions run in parallel, events must
/// not be mixed between sessions.
#[derive(Clone, Debug)]
pub enum AppEvent {
    StreamChunk {
        content: String,
        session_id: String,
    },
    /// Live prompt-processing progress (llama.cpp `prompt_progress` chunks,
    /// sent while the server processes the prompt before the first token).
    StreamPromptProgress {
        progress: PromptProgress,
        session_id: String,
    },
    /// An intermediate tool round finished (text committed, generation continues).
    StreamRoundComplete {
        content: String,
        usage: Option<Usage>,
        session_id: String,
    },
    StreamComplete {
        content: String,
        usage: Option<Usage>,
        session_id: String,
    },
    StreamError {
        error: String,
        session_id: String,
    },
    ToolCallWarning {
        tool_name: String,
        message: String,
        session_id: String,
    },
    /// A tool call began. `args_preview` is a one-line human-readable
    /// summary of the arguments (what the tool is doing), for the live
    /// tool card in the chat area.
    ToolCallStart {
        tool_name: String,
        call_id: String,
        args_preview: String,
        session_id: String,
    },
    /// Incremental progress for a running tool (e.g. the shell streams
    /// output lines). `text` is the LATEST tail of the output so far —
    /// the UI replaces (not appends) its display on each event. Throttled
    /// to a few events per second per tool.
    ToolCallProgress {
        tool_name: String,
        call_id: String,
        text: String,
        session_id: String,
    },
    ToolCallComplete {
        tool_name: String,
        call_id: String,
        result: String,
        session_id: String,
    },
    ToolCallError {
        tool_name: String,
        call_id: String,
        error: String,
        session_id: String,
    },
    // Thinking output events (e.g. Claude-style reasoning)
    StreamThinkingChunk {
        content: String,
        session_id: String,
    },
    StreamThinkingComplete {
        content: String,
        session_id: String,
    },
    /// Remote server n_ctx was updated.
    NCtxUpdated {
        n_ctx: u32,
        session_id: String,
    },
    /// Agent self-improvement suggestions generated.
    ImprovementSuggested {
        agent_name: String,
        suggestions: Vec<ImprovementSuggestion>,
        session_id: String,
    },
    /// 4b: a MANUAL "run check now" improvement check (the review-panel
    /// button) finished. `produced` = whether it yielded at least one
    /// suggestion; when it did, the suggestions themselves arrive separately
    /// via `ImprovementSuggested` (the same path as the per-task loop). This
    /// event is the "done" signal that clears the button's running state and
    /// reports the outcome (including "no suggestions"). No `session_id`: it
    /// is not session-bound — the improvements panel is global.
    ImprovementCheckFinished {
        agent_name: String,
        produced: bool,
    },
    /// The running agent (in `parent_session_id`) called `handoff(...,
    /// sub_session: true)`: the parent turn ended, and the UI must fork a
    /// CLEAN sub-session for `agent` (fresh store: only the target's system
    /// prompt + `task`, with its session meta carrying
    /// `parent_session_id = parent_session_id` — the `hand_back` link) and
    /// start its first turn with `task`. No `session_id` field: the UI
    /// routes on the explicit ids (the parent's generating state is already
    /// cleared by the parent's own terminal `StreamComplete`).
    SubSessionHandoff {
        parent_session_id: String,
        agent: String,
        task: String,
    },
    /// The running agent (in a sub-session) called the `hand_back` tool: the
    /// sub-session turn ended, and the UI must post `task` into
    /// `to_session_id` (the parent, resolved from the sub-session's session
    /// meta), where the original agent resumes with its full history.
    AgentHandBack {
        from_session_id: String,
        to_session_id: String,
        task: String,
    },
    /// The session switched agents: the running agent called the `handoff`
    /// tool and the target agent now continues the same conversation.
    AgentHandoff {
        from: String,
        to: String,
        task: String,
        session_id: String,
    },
    /// An MCP server configuration changed (a tool or the UI's MCP panel
    /// added / removed / updated a server). `session_id` is the session whose
    /// agent made the change (empty when the change came from the UI). The UI
    /// reloads its in-memory `config.mcp_servers` from disk and redraws the
    /// MCP panel so the panel and the live manager state stay in sync.
    McpConfigChanged {
        session_id: String,
    },
    /// The agent asked to restart the WuffAgent process (optionally after a
    /// build). The UI saves the session, writes a restart marker, relaunches
    /// the (optionally newly built) binary, and closes the window; the new
    /// process resumes this session automatically.
    RestartRequested {
        reason: String,
        build_cmd: Option<String>,
        exe_path: Option<String>,
        session_id: String,
    },
    /// A user message sent while this session's run was active arrived too
    /// late to be injected into the running agent loop (the loop had already
    /// ended — e.g. it landed during the final verification call — or the run
    /// was cancelled). The message was already displayed in the chat at send
    /// time, and `agent_prompt` / `tool_policy` were resolved at send time;
    /// the UI runs it as the next turn.
    UserMessageDrained {
        /// Boxed: QueuedMessage is the largest variant; boxing keeps
        /// AppEvent small (clippy::large_enum_variant).
        message: Box<QueuedMessage>,
        session_id: String,
    },
}
