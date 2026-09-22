use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A chat message with a role (system/user/assistant/tool) and content.
///
/// `image` holds an attached image as a `data:` URI (e.g.
/// `data:image/png;base64,...`). It is NOT stored as a separate JSON field:
/// on the wire (and in session files) an image message serializes its
/// `content` as the OpenAI multimodal parts array
/// (`[{"type":"text",...},{"type":"image_url",...}]`) so vision-capable
/// servers receive the standard format, and deserialization rebuilds
/// `content` (joined text parts) + `image` (first `image_url`) from it.
/// Messages without an image keep the plain string `content`.
// Serialize/Deserialize are implemented manually below (see `MessageDe`):
// `content` may be a plain string or a multimodal parts array, and serde
// derive cannot express that.
#[derive(Clone, Debug)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    /// Tool call requests from the AI (non-null when the AI wants to invoke a tool).
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Reference to the tool call this result belongs to (for tool role messages).
    pub tool_call_id: Option<String>,
    /// Model reasoning/thinking content (llama.cpp `reasoning_content`, DeepSeek/Qwen style).
    /// Round-tripped so the model can see its own prior reasoning across tool-call rounds.
    pub reasoning_content: Option<String>,
    /// Attached image as a `data:` URI (user messages with an image).
    /// Serialized inside `content` as an `image_url` part (see struct docs).
    pub image: Option<String>,
}

/// Wire form of [`Message`] where `content` stays raw so it can be either a
/// plain string (no image) or a JSON array of multimodal content parts.
#[derive(Deserialize)]
struct MessageDe {
    role: String,
    content: serde_json::Value,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Split the raw content value into plain text + attached image.
/// Accepts both the legacy plain-string form and the multimodal parts
/// array, so old session files keep loading unchanged.
fn split_content(content: serde_json::Value) -> (String, Option<String>) {
    match content {
        serde_json::Value::String(s) => (s, None),
        serde_json::Value::Array(parts) => {
            let mut text = String::new();
            let mut image = None;
            for part in parts {
                match part.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                    }
                    Some("image_url") => {
                        if image.is_none() {
                            image = part
                                .get("image_url")
                                .and_then(|u| u.get("url"))
                                .and_then(|u| u.as_str())
                                .map(|s| s.to_string());
                        }
                    }
                    _ => {}
                }
            }
            (text, image)
        }
        // Null or any other shape — keep it loadable, display as empty.
        _ => (String::new(), None),
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let de = MessageDe::deserialize(deserializer)?;
        let (content, image) = split_content(de.content);
        Ok(Message {
            role: de.role,
            content,
            timestamp: de.timestamp,
            tool_calls: de.tool_calls,
            tool_call_id: de.tool_call_id,
            reasoning_content: de.reasoning_content,
            image: image,
        })
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Message", 6)?;
        state.serialize_field("role", &self.role)?;
        if let Some(ref url) = self.image {
            // Multimodal content parts (OpenAI/llama.cpp vision format).
            let mut parts = Vec::new();
            if !self.content.is_empty() {
                parts.push(serde_json::json!({ "type": "text", "text": self.content }));
            }
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": url },
            }));
            state.serialize_field("content", &parts)?;
        } else {
            state.serialize_field("content", &self.content)?;
        }
        state.serialize_field("timestamp", &self.timestamp)?;
        if let Some(ref tool_calls) = self.tool_calls {
            state.serialize_field("tool_calls", tool_calls)?;
        }
        if let Some(ref tool_call_id) = self.tool_call_id {
            state.serialize_field("tool_call_id", tool_call_id)?;
        }
        if let Some(ref reasoning_content) = self.reasoning_content {
            state.serialize_field("reasoning_content", reasoning_content)?;
        }
        state.end()
    }
}

/// A single tool call requested by the AI.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolFunction,
}

/// Reasoning effort level sent to the model server (Qwen3/llama.cpp style).
/// Serialized as a lowercase string; `Off` is omitted from requests entirely.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// JSON string value for the request body, or `None` for Off.
    pub fn as_wire_value(self) -> Option<&'static str> {
        match self {
            ReasoningEffort::Off => None,
            ReasoningEffort::Low => Some("low"),
            ReasoningEffort::Medium => Some("medium"),
            ReasoningEffort::High => Some("xhigh"),
        }
    }

    /// Short name for dropdown items.
    pub fn name(self) -> &'static str {
        match self {
            ReasoningEffort::Off => "Off",
            ReasoningEffort::Low => "Low",
            ReasoningEffort::Medium => "Medium",
            ReasoningEffort::High => "High",
        }
    }

    /// Human-readable label for UI display.
    pub fn label(self) -> &'static str {
        match self {
            ReasoningEffort::Off => "Reasoning: Off",
            ReasoningEffort::Low => "Reasoning: Low",
            ReasoningEffort::Medium => "Reasoning: Medium",
            ReasoningEffort::High => "Reasoning: High",
        }
    }

    /// All selectable variants, in UI order.
    pub const VARIANTS: [ReasoningEffort; 4] = [
        ReasoningEffort::Off,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ];
}

/// The function specification within a tool call.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

/// Token usage statistics returned by the API.
#[derive(Deserialize, Debug, Clone)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// llama.cpp extension: server-reported per-stage speeds for the
    /// completed call. The server sends these in a `timings` field that is a
    /// SIBLING of `usage` on the wire; the client folds them in here (see
    /// `http.rs` / `sse.rs`) so the UI can show tokens/sec. `None` for
    /// backends that don't report timings.
    #[serde(default)]
    pub timings: Option<LlamaTimings>,
}

/// Live prompt-processing progress from the llama.cpp server: a
/// `prompt_progress` object in stream chunks (requested via
/// `return_progress: true` in the request body). Sent per server main-loop
/// tick while the prompt is being processed.
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct PromptProgress {
    /// Total prompt tokens for this call.
    pub total: u32,
    /// Tokens served from the KV cache (processed nearly for free).
    pub cache: u32,
    /// Tokens processed so far (including cached ones).
    pub processed: u32,
    /// Milliseconds elapsed since prompt processing started.
    pub time_ms: f64,
}

impl PromptProgress {
    /// Effective prompt-processing speed (tokens/s) counting only
    /// non-cached tokens. `None` while nothing new has been processed.
    pub fn prompt_tps(&self) -> Option<f64> {
        let new = f64::from(self.processed) - f64::from(self.cache);
        if self.time_ms < 1.0 || new < 1.0 {
            return None;
        }
        Some(new / (self.time_ms / 1000.0))
    }
}

/// llama.cpp server-reported per-stage speeds for one completed call.
#[derive(Deserialize, Debug, Clone)]
pub struct LlamaTimings {
    /// Prompt processing speed (tokens/second).
    #[serde(default)]
    pub prompt_per_second: Option<f64>,
    /// Token generation (llama.cpp "predicted") speed (tokens/second).
    #[serde(default)]
    pub predicted_per_second: Option<f64>,
}

/// UI application status indicator.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AppStatus {
    #[default]
    Stopped,
    Connecting,
    Ready,
    Generating,
    Error(String),
}

/// A chat message displayed in the UI.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Display kind — replaces legacy string-prefix conventions (💭 prefix, "||" tool format).
    #[serde(default)]
    pub kind: MessageKind,
}

/// How a UI chat message should be rendered.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    #[default]
    Normal,
    /// Model reasoning/thinking (rendered dim + italic).
    Thinking,
    /// Tool call result; content is "header||call_id||result_json"
    /// (plus an optional "||duration_ms" fourth part for calls made while
    /// the live tool-card UI is active) or a bare result.
    Tool,
}

/// Shell configuration for an agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Allowed command patterns (regex). Empty means allow all (except dangerous).
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// Shell type: "powershell", "cmd", or "bash".
    #[serde(default = "default_shell_type")]
    pub shell_type: String,
    /// Default timeout in milliseconds.
    #[serde(default = "default_shell_timeout")]
    pub shell_timeout_ms: u64,
    /// Whether shell commands are enabled.
    #[serde(default = "default_shell_enabled")]
    pub shell_enabled: bool,
    /// Working directory restriction (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            allowed_commands: Vec::new(),
            shell_type: "powershell".to_string(),
            shell_timeout_ms: 300_000,
            shell_enabled: false,
            working_dir: None,
        }
    }
}

fn default_shell_type() -> String {
    "powershell".to_string()
}
fn default_shell_timeout() -> u64 {
    300_000
}
fn default_shell_enabled() -> bool {
    false
}

/// A suggested improvement to an agent's configuration.
///
/// I2: beyond the prompt, the improver may now propose changes to any other
/// profile field. All new fields are optional and serde-defaulted, so old
/// suggestion JSON (and LLM responses that omit them) still parse.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImprovementSuggestion {
    pub agent_name: String,
    /// New prompt text, or None if no prompt change suggested.
    pub prompt_change: Option<String>,
    /// Explanation for why this improvement is suggested.
    pub rationale: String,
    /// Proposals for new specialized agents (LLMs commonly omit it when empty).
    #[serde(default)]
    pub new_agents: Vec<NewAgentProposal>,
    /// I2: replace the agent's tool allowlist (None = no change).
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// I2: change the agent's reasoning effort (None = no change).
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// I2: change the agent's shell configuration (None = no change).
    #[serde(default)]
    pub shell_config: Option<ShellConfig>,
    /// I2: change the agent's handoff target allowlist (None = no change).
    #[serde(default)]
    pub handoff_targets: Option<Vec<String>>,
    /// I2: change the per-task timeout in ms (None = no change).
    #[serde(default)]
    pub task_timeout_ms: Option<u64>,
    /// I3: the evidence the improver saw (trajectory line + lesson excerpts).
    /// Filled deterministically by `suggest_improvements`, not the LLM, so
    /// the review panel can show WHY the suggestion was made.
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// A proposal to create a new agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAgentProposal {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// Configuration for intelligent context trimming.
///
/// Tunable thresholds that control when and how aggressively tool
/// results and other content are summarized/truncated.
///
/// Lives in the types brick because `ChatToolPolicy` (also here, embedded in
/// `QueuedMessage`/`AppEvent`) carries a `TrimConfig`; keeping both in types
/// avoids a types→trimming edge (trimming→types already exists).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrimConfig {
    /// Whether trimming is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Hard cap on any tool result stored in messages.
    /// Results exceeding this are truncated regardless of type.
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,

    /// Maximum number of agent chain entries retained on session save.
    #[serde(default = "default_max_chain_entries")]
    pub max_chain_entries: usize,

    /// Maximum lines to keep for code blocks.
    #[serde(default = "default_code_max_lines")]
    pub code_max_lines: usize,

    /// Maximum lines to keep for build logs.
    #[serde(default = "default_log_max_lines")]
    pub log_max_lines: usize,

    /// Maximum items to keep for glob/file lists.
    #[serde(default = "default_list_max_items")]
    pub list_max_items: usize,

    /// Enable freshness-aware eviction of stale/superseded `read_file`
    /// results at trim time (see `filestate`): the whole tool pair is removed
    /// (no marker, no partial content), and no in-place shrink
    /// (summarize/halve) is ever applied to a `read_file` result, so the model
    /// never sees a partial file snapshot it could hallucinate lines from.
    /// Off = legacy behavior (one-line stale markers, size-based in-place
    /// summarization).
    #[serde(default = "default_true")]
    pub stale_file_invalidation: bool,
}

fn default_enabled() -> bool {
    true
}
fn default_max_tool_result_chars() -> usize {
    1000
}
fn default_max_chain_entries() -> usize {
    50
}
fn default_code_max_lines() -> usize {
    30
}
fn default_log_max_lines() -> usize {
    15
}
fn default_list_max_items() -> usize {
    20
}
fn default_true() -> bool {
    true
}

impl Default for TrimConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tool_result_chars: default_max_tool_result_chars(),
            max_chain_entries: default_max_chain_entries(),
            code_max_lines: default_code_max_lines(),
            log_max_lines: default_log_max_lines(),
            list_max_items: default_list_max_items(),
            stale_file_invalidation: default_true(),
        }
    }
}

impl TrimConfig {
    /// Returns true if trimming is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// The selected chat profile's tool policy, carried onto the chat path so the
/// chat agent runs with that profile's tool set, shell restrictions, handoff
/// rights, reasoning effort, and trim configuration (instead of the old
/// "all tools + allow-all shell" default).
#[derive(Clone, Debug)]
pub struct ChatToolPolicy {
    /// Tool names the profile authorizes. The chat agent is given all available
    /// tools plus `shell`, so an empty list does not strip tools from chat.
    pub allowed_tools: Vec<String>,
    /// The profile's shell config (allowlist/enabled/timeout).
    pub shell_config: ShellConfig,
    /// The selected profile's name (used for the chat agent's identity and
    /// handoff markers, e.g. "[Handoff from 'architect' to 'coder']").
    pub agent_name: String,
    /// Whether the chat agent may hand off the session via the `handoff` tool
    /// (the profile's `handoff_enabled` flag).
    pub handoff_enabled: bool,
    /// Agent names the profile may hand off to (empty = any enabled agent).
    pub handoff_targets: Vec<String>,
    /// Whether the chat agent may restart WuffAgent via the `restart` tool
    /// (the profile's `restart_enabled` flag).
    pub restart_enabled: bool,
    /// The profile's reasoning effort (Off = inherit the global toggle).
    pub reasoning_effort: ReasoningEffort,
    /// The profile's context-trimming configuration.
    pub trim_config: TrimConfig,
}

impl ChatToolPolicy {
    /// A permissive policy: all tools + an unrestricted (allow-all) shell.
    /// Used when no profile is selected or none is found.
    pub fn unrestricted() -> Self {
        Self {
            allowed_tools: Vec::new(),
            shell_config: ShellConfig {
                shell_enabled: true,
                allowed_commands: Vec::new(),
                ..ShellConfig::default()
            },
            agent_name: "chat".to_string(),
            // Permissive = may hand off to any enabled agent (empty targets).
            // Without this, "Auto" chat could never hand off to a profile,
            // even though every other tool is unrestricted.
            handoff_enabled: true,
            handoff_targets: Vec::new(),
            // Permissive = may restart (and rebuild) too.
            restart_enabled: true,
            reasoning_effort: ReasoningEffort::default(),
            trim_config: TrimConfig::default(),
        }
    }
}

/// A message sent while the AI is still working. Displayed in the chat
/// immediately and — via the pipeline's injection channel — handed to the
/// RUNNING agent loop, which injects it into the current turn at the next LLM
/// round boundary (the earliest point the model can see it), instead of
/// waiting for the whole run to finish.
///
/// The `queued_messages` fallback queue (see [`crate::sessions::ChatAreaState::queued_messages`])
/// holds a `QueuedMessage` only when injection was not possible (the run
/// already finished or was cancelled before delivery, or the message is
/// re-delivered while a new run is already in flight); those are processed as
/// the next turn once the current run (and any earlier queued messages)
/// finishes.
#[derive(Clone, Debug)]
pub struct QueuedMessage {
    pub text: String,
    /// Documented exception: the one egui type in the types brick. A
    /// data-URI migration (String instead of ImageSource) is deferred.
    pub image: Option<egui::ImageSource<'static>>,
    /// System prompt resolved from the selected agent at send time.
    pub agent_prompt: String,
    /// Tool policy (allowed_tools + shell config) resolved from the selected agent.
    pub tool_policy: ChatToolPolicy,
}

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
    /// The session switched agents: the running agent called the `handoff`
    /// tool and the target agent now continues the same conversation.
    AgentHandoff {
        from: String,
        to: String,
        task: String,
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
        /// Boxed: QueuedMessage (with its egui image payload) is the
        /// largest variant; boxing keeps AppEvent small
        /// (clippy::large_enum_variant).
        message: Box<QueuedMessage>,
        session_id: String,
    },
}

/// Convert an attached image (egui source from the UI) into the `data:` URI
/// form used in model requests. Only `Bytes` sources carry a payload to send;
/// texture/URI references have none and return `None`.
pub fn image_source_data_uri(source: &egui::ImageSource<'static>) -> Option<String> {
    let bytes = match source {
        egui::ImageSource::Bytes { bytes, .. } => bytes,
        _ => return None,
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes.as_ref());
    Some(format!("data:image/png;base64,{}", b64))
}

/// Format the current time as a human-readable timestamp string.
///
/// Includes the local date (`YYYY-MM-DD HH:MM:SS`) so the UI can draw day
/// separators. Display-only field — never parsed by the model layer. Legacy
/// sessions stored the older 8-char `HH:MM:SS` form; see `timestamp_day` /
/// `timestamp_time` for tolerant parsing.
pub fn format_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The `YYYY-MM-DD` day part of a display timestamp, if it has one.
/// Returns `None` for legacy time-only timestamps (no separator is drawn).
pub fn timestamp_day(ts: &str) -> Option<&str> {
    if ts.len() >= 11 && ts.as_bytes()[4] == b'-' {
        Some(&ts[..10])
    } else {
        None
    }
}

/// The time part of a display timestamp for rendering, or the whole string
/// for legacy time-only timestamps.
pub fn timestamp_time(ts: &str) -> &str {
    if ts.len() >= 19 && ts.as_bytes()[10] == b' ' {
        &ts[11..]
    } else {
        ts
    }
}

/// Format a tool call header for display.
pub fn tool_call_header(name: &str, result: &str) -> String {
    format!(
        "🔧 {}: {}",
        name,
        result.chars().take(80).collect::<String>()
    )
}

/// One-line human-readable preview of a tool call's arguments, for the live
/// tool cards in the UI ("what is the tool doing right now?").
///
/// Picks the most descriptive argument field per tool; falls back to a
/// compact JSON dump. Empty string for empty / unknown argument shapes.
pub fn tool_args_summary(name: &str, arguments: &str) -> String {
    const MAX: usize = 120;
    let trimmed = arguments.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let flat = |s: &str| -> String {
        let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out: String = one_line.chars().take(MAX).collect();
        if one_line.chars().count() > MAX {
            out.push('…');
        }
        out
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // Preferred argument fields per tool (first present field wins).
        let fields: &[&str] = match name {
            "shell" => &["command"],
            "read_file" | "append_file" | "apply_diff" | "write_file" | "delete" | "file_info"
            | "mkdir" | "list_dir" => &["path"],
            "copy" | "move" => &["dest"],
            "search_files" => &["pattern"],
            "search_content" => &["pattern", "path"],
            "web_search" | "search_memory" => &["query"],
            "fetch_url" => &["url"],
            "calculation" => &["expression"],
            "save_memory" | "update_memory" | "consolidate_memories" => &["content"],
            "handoff" => &["task"],
            "restart" => &["reason"],
            _ => &[
                "command",
                "path",
                "query",
                "url",
                "pattern",
                "expression",
                "content",
                "reason",
                "task",
            ],
        };
        for f in fields {
            if let Some(s) = v.get(f).and_then(|x| x.as_str()) {
                if s.trim().is_empty() {
                    continue;
                }
                // search_content: show "pattern in path" when both are given.
                if name == "search_content" && *f == "pattern" {
                    if let Some(p) = v.get("path").and_then(|x| x.as_str()) {
                        if !p.trim().is_empty() {
                            return flat(&format!("{} in {}", s, p));
                        }
                    }
                }
                return flat(s);
            }
        }
        return flat(&v.to_string());
    }
    flat(trimmed)
}

#[cfg(test)]
mod tests;
