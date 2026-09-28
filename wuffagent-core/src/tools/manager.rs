use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::tools::registry::{ToolEntry, ToolRegistry};
use crate::tools::types::{
    JsonSchema, Tool, ToolError, ToolLogger, ToolMetadata, ToolOutput, ToolParams, ToolProgress,
    ToolResult, TracingToolLogger,
};
use crate::types::Message;

/// Parse raw tool-call argument JSON into `ToolParams`.
///
/// Tries the direct `ToolParams` shape first, then wraps a plain JSON object's
/// fields into `values` (some models emit direct arguments like
/// `{"path": "..."}`).
pub fn parse_tool_args(arguments: &str) -> Result<ToolParams, String> {
    let args = serde_json::from_str::<serde_json::Value>(arguments).map_err(|_| {
        format!(
            "Failed to parse arguments: {}",
            &arguments[..arguments.len().min(100)]
        )
    })?;
    // Try deserializing directly first; fall back to raw values map.
    if let Ok(p) = serde_json::from_value::<ToolParams>(args.clone()) {
        return Ok(p);
    }
    let mut values = std::collections::HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            values.insert(k.clone(), v.clone());
        }
    }
    Ok(ToolParams { values })
}

/// Whether raw tool-call arguments form a complete JSON object.
///
/// A model that hits its max-output-token limit mid-argument leaves truncated
/// JSON behind (unterminated string, unbalanced braces). Such arguments must
/// never be persisted or replayed to the server: OpenAI-compatible servers
/// parse every tool call in the conversation history on each request and
/// reject the whole request with HTTP 500 ("failed to parse tool call
/// arguments") when they find an incomplete one.
pub fn tool_args_complete(arguments: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(arguments),
        Ok(serde_json::Value::Object(_))
    )
}

/// Repair a message's tool calls in place: the arguments of any call that is
/// not a complete JSON object are replaced with `{}` so the message can be
/// stored and replayed to the server safely.
///
/// Returns the ids of the repaired (i.e. truncated) calls so callers can
/// report the truncation to the model instead of executing them.
pub fn repair_truncated_tool_calls(message: &mut Message) -> Vec<String> {
    let Some(calls) = message.tool_calls.as_mut() else {
        return Vec::new();
    };
    let mut repaired = Vec::new();
    for call in calls.iter_mut() {
        if !tool_args_complete(&call.function.arguments) {
            repaired.push(call.id.clone());
            call.function.arguments = "{}".to_string();
        }
    }
    repaired
}

/// Per-run view over the shared registry: tool replacements (a per-agent
/// shell, per-execution handoff/restart/hand_back/session_note) and removals.
/// Shares the registry — no copying of entries.
///
/// Invariant: a name is in at most one of `replaced`/`removed` —
/// [`ToolManager::with_override`] moves it between the two, never adds it to both.
#[derive(Clone, Default)]
pub struct ToolOverrides {
    /// Entries replacing the registry's tool of the same name (keyed by `metadata.name`).
    replaced: Vec<ToolEntry>,
    /// Names hidden from the view (absent from the registry AND from `replaced`).
    removed: Vec<String>,
}

impl ToolOverrides {
    /// The replacement entry for a tool name, if any.
    fn replaced(&self, name: &str) -> Option<&ToolEntry> {
        self.replaced.iter().find(|e| e.metadata.name == name)
    }

    /// Whether a tool name is removed from the view.
    fn is_removed(&self, name: &str) -> bool {
        self.removed.iter().any(|r| r == name)
    }
}

/// High-level orchestrator that exposes tool execution to the rest of the application.
pub struct ToolManager {
    registry: Arc<ToolRegistry>,
    logger: Arc<dyn ToolLogger>,
    allowlist: Option<Vec<String>>,
    /// Discovery paths the shared registry scans (T3b). Shared with every
    /// manager built from this one so `add_discovery_path` reaches them all.
    discovery_paths: Arc<Mutex<Vec<PathBuf>>>,
    /// Per-run tool overrides (replacements/removals) layered on the registry.
    overrides: ToolOverrides,
}

impl Clone for ToolManager {
    fn clone(&self) -> Self {
        // `derive` clones every field (all state is shared through Arcs).
        self.derive()
    }
}

impl ToolManager {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        let discovery_paths = Arc::new(Mutex::new(registry.discovery_paths()));
        Self {
            registry,
            logger: Arc::new(TracingToolLogger),
            allowlist: None,
            discovery_paths,
            overrides: ToolOverrides::default(),
        }
    }

    /// Create a ToolManager with an empty registry (no tools).
    pub fn new_empty() -> Self {
        let registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        Self {
            registry,
            logger: Arc::new(TracingToolLogger),
            allowlist: None,
            discovery_paths: Arc::new(Mutex::new(Vec::new())),
            overrides: ToolOverrides::default(),
        }
    }

    /// A shallow copy of this manager (all state shared) — the base for the
    /// `with_*` variants below.
    fn derive(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
            overrides: self.overrides.clone(),
        }
    }

    /// Create a new ToolManager that shares the same registry but restricts tools to the given allowlist.
    pub fn with_allowlist(&self, names: &[String]) -> Self {
        let mut s = self.derive();
        s.allowlist = Some(names.to_vec());
        s
    }

    /// Apply one per-run override: `Some(entry)` replaces the tool with that
    /// name (re-adding it if an earlier step removed it), `None` removes the
    /// name (including any earlier per-run replacement). Shares the registry.
    fn with_override(&self, name: &str, entry: Option<ToolEntry>) -> Self {
        let replaced = entry.is_some();
        let mut s = self.derive();
        match entry {
            Some(e) => {
                s.overrides.replaced.retain(|r| r.metadata.name != name);
                s.overrides.removed.retain(|r| r != name);
                s.overrides.replaced.push(e);
            }
            None => {
                s.overrides.replaced.retain(|r| r.metadata.name != name);
                if !s.overrides.removed.iter().any(|r| r == name) {
                    s.overrides.removed.push(name.to_string());
                }
            }
        }
        tracing::debug!(
            name,
            replaced,
            override_count = s.overrides.replaced.len() + s.overrides.removed.len(),
            "Applying per-run tool override"
        );
        s
    }

    /// The metadata in effect for a tool name in this manager's view: a
    /// per-run replacement (including one applied by an ancestor manager in
    /// the chain), then the shared registry, then a synthesized default.
    fn effective_metadata(&self, name: &str, default_description: &str) -> ToolMetadata {
        self.overrides
            .replaced(name)
            .map(|e| e.metadata.clone())
            .or_else(|| self.registry.metadata_for(name))
            .unwrap_or_else(|| ToolMetadata {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: default_description.to_string(),
                dependencies: vec![],
            })
    }

    /// Build the override entry for a per-execution tool (metadata from
    /// [`Self::effective_metadata`], so chained `with_*` calls keep it stable).
    fn override_entry<T: Tool + 'static>(&self, name: &str, tool: T, default_description: &str) -> ToolEntry {
        ToolEntry {
            tool: Arc::new(tool),
            metadata: self.effective_metadata(name, default_description),
            loaded_at: Instant::now(),
            plugin: None,
        }
    }

    /// Create a new ToolManager whose `shell` tool honors the given per-agent
    /// shell config (allowlist/enabled/timeout), while all other tools and the
    /// allowlist are preserved. This is how an agent gets a shell restricted to
    /// its own allowed commands instead of the shared allow-all shell.
    pub fn with_shell_config(&self, cfg: crate::agents::config::ShellConfig) -> Self {
        let shell = crate::tools::builtin::shell::ShellTool::new(
            crate::tools::builtin::shell::ShellConfig::from(cfg),
        );
        self.with_override(
            "shell",
            Some(self.override_entry(
                "shell",
                shell,
                "Execute shell commands on the local system",
            )),
        )
    }

    /// Create a new ToolManager whose `handoff` entry is the provided
    /// per-execution tool (wired to this agent's mailbox, agents dir, and
    /// target allowlist). Same override mechanism as [`Self::with_shell_config`].
    pub fn with_handoff_tool(&self, tool: crate::tools::builtin::handoff::HandoffTool) -> Self {
        self.with_override(
            "handoff",
            Some(self.override_entry(
                "handoff",
                tool,
                "Hand off the session to another agent",
            )),
        )
    }

    /// Create a new ToolManager whose `restart` entry is the provided
    /// per-execution tool (wired to this agent's mailbox).
    pub fn with_restart_tool(&self, tool: crate::tools::builtin::restart::RestartTool) -> Self {
        self.with_override(
            "restart",
            Some(self.override_entry(
                "restart",
                tool,
                "Restart WuffAgent (optionally after a build) and resume the session",
            )),
        )
    }

    /// Create a new ToolManager whose `hand_back` entry is the provided
    /// per-execution tool (wired to this agent's mailbox; only sub-session
    /// agents get one).
    pub fn with_hand_back_tool(&self, tool: crate::tools::builtin::hand_back::HandBackTool) -> Self {
        self.with_override(
            "hand_back",
            Some(self.override_entry(
                "hand_back",
                tool,
                "Return this sub-session to its parent session",
            )),
        )
    }

    /// Create a new ToolManager whose `session_note` entry is the provided
    /// per-execution tool (wired to this agent's mailbox; `run_llm_loop`
    /// inserts the pinned note before the next LLM round).
    pub fn with_session_note_tool(
        &self,
        tool: crate::tools::builtin::session_note::SessionNoteTool,
    ) -> Self {
        self.with_override(
            "session_note",
            Some(self.override_entry(
                "session_note",
                tool,
                "Pin a short session state note (survives trims and reloads)",
            )),
        )
    }

    /// Remove the `handoff` tool from the schema (agents with `handoff_enabled` false).
    pub fn without_handoff(&self) -> Self {
        self.with_override("handoff", None)
    }

    /// Remove the `hand_back` tool from the schema (agents without a sub-session parent link).
    pub fn without_hand_back(&self) -> Self {
        self.with_override("hand_back", None)
    }

    /// Remove the `restart` tool from the schema (agents with `restart_enabled` false).
    pub fn without_restart(&self) -> Self {
        self.with_override("restart", None)
    }

    /// Remove the `shell` tool from the schema (agents with `shell_enabled` false).
    pub fn without_shell(&self) -> Self {
        self.with_override("shell", None)
    }

    /// Resolve a tool by name in this manager's view: a per-run replacement
    /// first, then the shared registry (skipping removed names).
    fn resolve_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(e) = self.overrides.replaced(name) {
            return Some(e.tool.clone());
        }
        if self.overrides.is_removed(name) {
            return None;
        }
        self.registry.get(name)
    }

    /// The input schema in effect for a tool name (a replacement's schema
    /// shadows the registry's; a removed or unknown name has none).
    fn schema_for(&self, name: &str) -> Option<JsonSchema> {
        if let Some(e) = self.overrides.replaced(name) {
            return e.tool.parameters_schema().input_type;
        }
        if self.overrides.is_removed(name) {
            return None;
        }
        self.registry.schema_for(name)
    }

    /// Execute a tool by name with the given parameters.
    ///
    /// Delegates to [`Self::execute_with_progress`] with a no-op sink.
    pub async fn execute(&self, tool_name: &str, params: ToolParams) -> ToolResult<ToolOutput> {
        self.execute_with_progress(tool_name, params, &ToolProgress::none())
            .await
    }

    /// Execute a tool by name, forwarding incremental progress reports
    /// (e.g. live shell output) to the given sink.
    pub async fn execute_with_progress(
        &self,
        tool_name: &str,
        params: ToolParams,
        progress: &ToolProgress,
    ) -> ToolResult<ToolOutput> {
        // Check allowlist first
        if let Some(ref allowlist) = self.allowlist {
            if !allowlist.contains(&tool_name.to_string()) {
                return Err(ToolError::NotFound(tool_name.to_string()));
            }
        }

        // Get the tool and execute it atomically (per-run overrides first).
        let tool = self
            .resolve_tool(tool_name)
            .ok_or_else(|| ToolError::NotFound(tool_name.to_string()))?;

        // (T5/M2) Validate the parameters against the tool's declared schema
        // BEFORE executing: `InvalidParams` is fixable, so the agent loop can
        // feed the named problems back to the model for a corrected call.
        if let Err(e) = self.validate(tool_name, &params) {
            return Err(e);
        }

        self.logger.log_tool_call(tool_name, &params);

        // Clone the sink so it can be moved into the blocking task.
        let progress = progress.clone();
        // Execute on the blocking thread to avoid holding the main runtime.
        let result =
            tokio::task::spawn_blocking(move || tool.execute_with_progress(params, &progress))
                .await
                .map_err(|e| ToolError::Execution(format!("Join error: {}", e)))?;

        match &result {
            Ok(output) => self.logger.log_tool_result(tool_name, output),
            Err(e) => self.logger.log_tool_error(tool_name, e),
        }

        result
    }

    /// Validate parameters against the tool's declared input schema (T5/M2):
    /// required fields must be present (and non-null unless the field is
    /// nullable), and declared top-level property types must match.
    ///
    /// Returns [`ToolError::InvalidParams`] naming every problem (joined), so
    /// the LLM can self-correct. Tools that declare no input schema are always
    /// valid — their own argument parsing is the final authority.
    pub fn validate(&self, tool_name: &str, params: &ToolParams) -> ToolResult<()> {
        let Some(schema) = self.schema_for(tool_name) else {
            return Ok(());
        };
        let problems = crate::tools::validation::validate_params(&schema, params);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ToolError::InvalidParams(problems.join("; ")))
        }
    }

    /// Get all tool definitions in OpenAI-compatible format for function calling.
    /// Merges per-run overrides over the registry, then filters by allowlist
    /// when present (the filter applies after the merge, as before).
    pub fn get_tool_definitions(&self) -> Vec<crate::tools::types::ToolDefinition> {
        let mut defs = self.registry.to_tool_definitions();
        // Remove entries that a per-run override removed.
        if !self.overrides.removed.is_empty() {
            defs.retain(|d| !self.overrides.is_removed(&d.function.name));
        }
        // Replace (or add) entries for per-run replacements.
        for e in &self.overrides.replaced {
            let def = ToolRegistry::definition_for(&e.tool);
            if let Some(slot) = defs.iter_mut().find(|d| d.function.name == e.metadata.name) {
                *slot = def;
            } else {
                defs.push(def);
            }
        }
        if let Some(ref allowlist) = self.allowlist {
            defs.retain(|d| allowlist.contains(&d.function.name));
        }
        defs
    }

    /// Get the list of allowed tool names, if an allowlist is set.
    /// Without an allowlist: the registry's names with per-run overrides applied.
    pub fn get_allowed_tools(&self) -> Vec<String> {
        if let Some(ref allowlist) = self.allowlist {
            allowlist.clone()
        } else {
            let mut names = self.registry.names();
            if !self.overrides.removed.is_empty() {
                names.retain(|n| !self.overrides.is_removed(n));
            }
            for e in &self.overrides.replaced {
                if !names.contains(&e.metadata.name) {
                    names.push(e.metadata.name.clone());
                }
            }
            names
        }
    }

    /// Add a discovery path (shared with every manager built from this one)
    /// and rescan for plugins (T3b). Returns the number of plugins newly
    /// loaded by the rescan.
    pub fn add_discovery_path(&self, path: PathBuf) -> ToolResult<usize> {
        let mut shared = self.discovery_paths.lock().unwrap();
        if !shared.iter().any(|p| p == &path) {
            shared.push(path.clone());
        }
        drop(shared);
        self.registry.add_discovery_path(path);
        self.registry.discover_plugins()
    }

    /// Remove a tool by name.
    pub fn remove_tool(&self, name: &str) -> ToolResult<()> {
        self.registry.unregister(name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
