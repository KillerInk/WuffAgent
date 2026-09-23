//! Tool policy and shell configuration types carried from the selected
//! chat profile onto the chat path, plus improvement-suggestion types and
//! the mid-run `QueuedMessage`.

use serde::{Deserialize, Serialize};

use super::ReasoningEffort;

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
    /// Attached image as a `data:` URI (`data:image/png;base64,...`), or
    /// `None`. The egui layer converts the attached `ImageSource` to this
    /// form before the message crosses into core.
    pub image: Option<String>,
    /// System prompt resolved from the selected agent at send time.
    pub agent_prompt: String,
    /// Tool policy (allowed_tools + shell config) resolved from the selected agent.
    pub tool_policy: ChatToolPolicy,
}
