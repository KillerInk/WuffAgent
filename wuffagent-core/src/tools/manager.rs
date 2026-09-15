use std::path::PathBuf;
use std::sync::Arc;

use crate::tools::types::{
    ToolError, ToolLogger, ToolOutput, ToolParams, ToolResult, TracingToolLogger,
};
use crate::tools::registry::ToolRegistry;

/// Parse raw tool-call argument JSON into `ToolParams`.
///
/// Tries the direct `ToolParams` shape first, then wraps a plain JSON object's
/// fields into `values` (some models emit direct arguments like
/// `{"path": "..."}`).
pub fn parse_tool_args(arguments: &str) -> Result<ToolParams, String> {
    let args = serde_json::from_str::<serde_json::Value>(arguments)
        .map_err(|_| format!("Failed to parse arguments: {}", &arguments[..arguments.len().min(100)]))?;
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

/// High-level orchestrator that exposes tool execution to the rest of the application.
pub struct ToolManager {
    registry: Arc<ToolRegistry>,
    logger: Arc<dyn ToolLogger>,
    allowlist: Option<Vec<String>>,
}

impl Clone for ToolManager {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
        }
    }
}

impl ToolManager {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            logger: Arc::new(TracingToolLogger),
            allowlist: None,
        }
    }

    /// Create a ToolManager with an empty registry (no tools).
    pub fn new_empty() -> Self {
        let registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
        Self {
            registry,
            logger: Arc::new(TracingToolLogger),
            allowlist: None,
        }
    }

    /// Create a new ToolManager that shares the same registry but restricts tools to the given allowlist.
    pub fn with_allowlist(&self, names: &[String]) -> Self {
        Self {
            registry: self.registry.clone(),
            logger: self.logger.clone(),
            allowlist: Some(names.to_vec()),
        }
    }

    /// Rebuild a registry from the current one, swapping the shared `shell` tool
    /// for a `ShellTool` built from the given per-agent config. Pass `None` to
    /// keep the shared shell unchanged.
    fn rebuild_registry(&self, shell_cfg: Option<crate::agents::config::ShellConfig>) -> Arc<ToolRegistry> {
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
        let registry = ToolRegistry::new(vec![], self.logger.clone());
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
        let registry = ToolRegistry::new(vec![], self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
        }
    }

    /// Create a new ToolManager where the `handoff` tool is removed from the
    /// schema entirely. Used for agents whose `handoff_enabled` is false —
    /// including target agents in a handoff chain built on top of a manager
    /// that already carries a per-execution handoff tool.
    pub fn without_handoff(&self) -> Self {
        let mut entries = self.registry.list();
        entries.retain(|e| e.metadata.name != "handoff");
        let registry = ToolRegistry::new(vec![], self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
        }
    }

    /// Create a new ToolManager where the `shell` tool is removed from the
    /// schema entirely. Used for agents whose `shell_enabled` is false, so
    /// the model never sees a shell tool whose calls would always fail.
    pub fn without_shell(&self) -> Self {
        let mut entries = self.registry.list();
        entries.retain(|e| e.metadata.name != "shell");
        let registry = ToolRegistry::new(vec![], self.logger.clone());
        for entry in entries {
            let _ = registry.register(entry);
        }
        Self {
            registry: std::sync::Arc::new(registry),
            logger: self.logger.clone(),
            allowlist: self.allowlist.clone(),
        }
    }

    /// Execute a tool by name with the given parameters.
    pub async fn execute(
        &self,
        tool_name: &str,
        params: ToolParams,
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

        // Execute on the blocking thread to avoid holding the main runtime.
        let result = tokio::task::spawn_blocking(move || tool.execute(params))
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
            self.registry.list().iter().map(|e| e.metadata.name.clone()).collect()
        }
    }

    /// Add a discovery path and rescan for plugins.
    pub fn add_discovery_path(&self, path: PathBuf) -> ToolResult<usize> {
        // The registry currently uses a RwLock; we need to add paths dynamically.
        // For now we rely on the initial paths passed at construction.
        let _ = path;
        self.registry.discover_plugins()
    }

    /// Remove a tool by name.
    pub fn remove_tool(&self, name: &str) -> ToolResult<()> {
        self.registry.unregister(name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::types::ToolMetadata;
    use crate::tools::registry::ToolEntry;

    /// Build a ToolManager whose registry contains a single `shell` entry,
    /// mirroring the global registration in `register_builtins`.
    fn manager_with_shell() -> ToolManager {
        let registry = ToolRegistry::new(vec![], Arc::new(TracingToolLogger));
        registry
            .register(ToolEntry {
                tool: Arc::new(crate::tools::builtin::shell::ShellTool::new(
                    crate::tools::builtin::shell::ShellConfig {
                        enabled: true,
                        ..Default::default()
                    },
                )),
                metadata: ToolMetadata {
                    name: "shell".to_string(),
                    version: "1.0.0".to_string(),
                    description: "Execute shell commands on the local system".to_string(),
                    dependencies: vec![],
                },
                loaded_at: std::time::Instant::now(),
            })
            .unwrap();
        ToolManager::new(Arc::new(registry))
    }

    #[test]
    fn test_without_shell_removes_shell_from_schema() {
        let tm = manager_with_shell();
        assert!(tm.get_allowed_tools().contains(&"shell".to_string()));

        let tm = tm.without_shell();
        let names = tm.get_allowed_tools();
        assert!(
            !names.contains(&"shell".to_string()),
            "shell should be removed from the schema: {:?}",
            names
        );
        assert!(tm.get_tool_definitions().is_empty());
    }

    #[test]
    fn test_with_handoff_tool_swaps_entry() {
        use crate::tools::builtin::handoff::HandoffTool;
        let tm = manager_with_shell();
        assert!(
            !tm.get_allowed_tools().contains(&"handoff".to_string()),
            "fresh manager has no handoff tool"
        );

        let mailbox = Arc::new(std::sync::Mutex::new(None));
        let tool = HandoffTool::new(
            mailbox,
            std::path::PathBuf::from("does-not-matter"),
            Vec::new(),
            Vec::new(),
        );
        let tm = tm.with_handoff_tool(tool);

        let names = tm.get_allowed_tools();
        assert!(names.contains(&"handoff".to_string()), "handoff added: {:?}", names);
        assert!(names.contains(&"shell".to_string()), "other tools preserved: {:?}", names);
        // Exactly one handoff entry, and it is the per-execution one.
        let defs: Vec<_> = tm
            .get_tool_definitions()
            .into_iter()
            .filter(|d| d.function.name == "handoff")
            .collect();
        assert_eq!(defs.len(), 1);
    }
}