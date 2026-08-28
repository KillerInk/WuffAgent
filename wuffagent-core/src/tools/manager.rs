use std::path::PathBuf;
use std::sync::Arc;

use crate::tools::types::{ToolError, ToolLogger, ToolOutput, ToolParams, ToolResult, TracingToolLogger};
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