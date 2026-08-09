use std::path::PathBuf;
use std::sync::Arc;

use crate::tools::lib::{ToolError, ToolLogger, ToolOutput, ToolParams, ToolResult, TracingToolLogger};
use crate::tools::registry::ToolRegistry;

/// High-level orchestrator that exposes tool execution to the rest of the application.
pub struct ToolManager {
    registry: Arc<ToolRegistry>,
    logger: Arc<dyn ToolLogger>,
}

impl ToolManager {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            logger: Arc::new(TracingToolLogger),
        }
    }

    /// Execute a tool by name with the given parameters.
    pub async fn execute(
        &self,
        tool_name: &str,
        params: ToolParams,
    ) -> ToolResult<ToolOutput> {
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
    pub fn get_tool_definitions(&self) -> Vec<crate::tools::lib::ToolDefinition> {
        self.registry.to_tool_definitions()
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
