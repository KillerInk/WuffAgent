use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::tools::registry::ToolRegistry;
use crate::tools::types::{
    ToolError, ToolLogger, ToolOutput, ToolParams, ToolProgress, ToolResult, TracingToolLogger,
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

/// High-level orchestrator that exposes tool execution to the rest of the application.
pub struct ToolManager {
    registry: Arc<ToolRegistry>,
    logger: Arc<dyn ToolLogger>,
    allowlist: Option<Vec<String>>,
    /// Discovery paths the ORIGINAL registry scans (T3b). Shared with every
    /// manager built from this one so a rebuilt registry keeps scanning the
    /// same plugin directories.
    discovery_paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl Clone for ToolManager {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
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
        }
    }

    /// Create a new ToolManager that shares the same registry but restricts tools to the given allowlist.
    pub fn with_allowlist(&self, names: &[String]) -> Self {
        Self {
            registry: self.registry.clone(),
            logger: self.logger.clone(),
            allowlist: Some(names.to_vec()),
            discovery_paths: self.discovery_paths.clone(),
        }
    }

    /// The shared discovery-path list (a copy).
    fn discovery_paths(&self) -> Vec<PathBuf> {
        self.discovery_paths.lock().unwrap().clone()
    }

    /// Rebuild a registry from the current one, swapping the shared `shell` tool
    /// for a `ShellTool` built from the given per-agent config. Pass `None` to
    /// keep the shared shell unchanged.
    fn rebuild_registry(
        &self,
        shell_cfg: Option<crate::agents::config::ShellConfig>,
    ) -> Arc<ToolRegistry> {
        let mut entries = self.registry.list();
        if let Some(cfg) = shell_cfg {
            let new_shell = crate::tools::builtin::shell::ShellTool::new(
                crate::tools::builtin::shell::ShellConfig::from(cfg),
            );
            let meta = entries
                .iter()
                .find(|e| e.metadata.name == "shell")
                .map(|e| e.metadata.clone())
                .unwrap_or_else(|| crate::tools::types::ToolMetadata {
                    name: "shell".to_string(),
                    version: "1.0.0".to_string(),
                    description: "Execute shell commands on the local system".to_string(),
                    dependencies: vec![],
                });
            entries.retain(|e| e.metadata.name != "shell");
            entries.push(crate::tools::registry::ToolEntry {
                tool: std::sync::Arc::new(new_shell),
                metadata: meta,
                loaded_at: std::time::Instant::now(),
            });
        }
        let registry = ToolRegistry::new(self.discovery_paths(), self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Arc::new(registry)
    }

    /// Create a new ToolManager whose `shell` tool honors the given per-agent
    /// shell config (allowlist/enabled/timeout), while all other tools and the
    /// allowlist are preserved. This is how an agent gets a shell restricted to
    /// its own allowed commands instead of the shared allow-all shell.
    pub fn with_shell_config(&self, cfg: crate::agents::config::ShellConfig) -> Self {
        Self {
            registry: self.rebuild_registry(Some(cfg)),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
    }

    /// Create a new ToolManager whose `handoff` entry is replaced by the
    /// provided per-execution tool (same rebuild pattern as
    /// [`Self::with_shell_config`]). Used by `Agent::new` to give
    /// `handoff_enabled` agents a handoff tool wired to their own mailbox,
    /// agents dir, and target allowlist.
    pub fn with_handoff_tool(&self, tool: crate::tools::builtin::handoff::HandoffTool) -> Self {
        let mut entries = self.registry.list();
        let meta = entries
            .iter()
            .find(|e| e.metadata.name == "handoff")
            .map(|e| e.metadata.clone())
            .unwrap_or_else(|| crate::tools::types::ToolMetadata {
                name: "handoff".to_string(),
                version: "1.0.0".to_string(),
                description: "Hand off the session to another agent".to_string(),
                dependencies: vec![],
            });
        entries.retain(|e| e.metadata.name != "handoff");
        entries.push(crate::tools::registry::ToolEntry {
            tool: std::sync::Arc::new(tool),
            metadata: meta,
            loaded_at: std::time::Instant::now(),
        });
        let registry = ToolRegistry::new(self.discovery_paths(), self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
    }

    /// Create a new ToolManager whose `restart` entry is replaced by the
    /// provided per-execution tool (same rebuild pattern as
    /// [`Self::with_handoff_tool`]). Used by `Agent::new` to give
    /// `restart_enabled` agents a restart tool wired to their own mailbox.
    pub fn with_restart_tool(&self, tool: crate::tools::builtin::restart::RestartTool) -> Self {
        let mut entries = self.registry.list();
        let meta = entries
            .iter()
            .find(|e| e.metadata.name == "restart")
            .map(|e| e.metadata.clone())
            .unwrap_or_else(|| crate::tools::types::ToolMetadata {
                name: "restart".to_string(),
                version: "1.0.0".to_string(),
                description: "Restart WuffAgent (optionally after a build) and resume the session"
                    .to_string(),
                dependencies: vec![],
            });
        entries.retain(|e| e.metadata.name != "restart");
        entries.push(crate::tools::registry::ToolEntry {
            tool: std::sync::Arc::new(tool),
            metadata: meta,
            loaded_at: std::time::Instant::now(),
        });
        let registry = ToolRegistry::new(self.discovery_paths(), self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
    }

    /// Create a new ToolManager where the `handoff` tool is removed from the
    /// schema entirely. Used for agents whose `handoff_enabled` is false —
    /// including target agents in a handoff chain built on top of a manager
    /// that already carries a per-execution handoff tool.
    pub fn without_handoff(&self) -> Self {
        let mut entries = self.registry.list();
        entries.retain(|e| e.metadata.name != "handoff");
        let registry = ToolRegistry::new(self.discovery_paths(), self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
    }

    /// Create a new ToolManager where the `restart` tool is removed from the
    /// schema entirely. Used for agents whose `restart_enabled` is false.
    pub fn without_restart(&self) -> Self {
        let mut entries = self.registry.list();
        entries.retain(|e| e.metadata.name != "restart");
        let registry = ToolRegistry::new(self.discovery_paths(), self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
    }

    /// Create a new ToolManager where the `shell` tool is removed from the
    /// schema entirely. Used for agents whose `shell_enabled` is false, so
    /// the model never sees a shell tool whose calls would always fail.
    pub fn without_shell(&self) -> Self {
        let mut entries = self.registry.list();
        entries.retain(|e| e.metadata.name != "shell");
        let registry = ToolRegistry::new(self.discovery_paths(), self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
            discovery_paths: self.discovery_paths.clone(),
        }
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

        // Get the tool and execute it atomically
        let tool = self
            .registry
            .get(tool_name)
            .ok_or_else(|| ToolError::NotFound(tool_name.to_string()))?;

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

    /// Validate parameters against the tool's schema.
    ///
    /// For now this is a no-op placeholder; a real implementation would
    /// validate the JSON schema using a library such as `jsonschema`.
    pub fn validate(&self, tool_name: &str, _params: &ToolParams) -> ToolResult<()> {
        let _ = tool_name;
        let _ = _params;
        // TODO: implement proper JSON schema validation
        Ok(())
    }

    /// Get all tool definitions in OpenAI-compatible format for function calling.
    /// Filters by allowlist when present.
    pub fn get_tool_definitions(&self) -> Vec<crate::tools::types::ToolDefinition> {
        let mut defs = self.registry.to_tool_definitions();
        if let Some(ref allowlist) = self.allowlist {
            defs.retain(|d| allowlist.contains(&d.function.name));
        }
        defs
    }

    /// Get the list of allowed tool names, if an allowlist is set.
    pub fn get_allowed_tools(&self) -> Vec<String> {
        if let Some(ref allowlist) = self.allowlist {
            allowlist.clone()
        } else {
            self.registry
                .list()
                .iter()
                .map(|e| e.metadata.name.clone())
                .collect()
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
