//! Runtime plugin management tools (T3b): `reload_plugins` and
//! `add_plugin_path`.
//!
//! Both are SHARED-REGISTRY tools (no per-run state): they wrap the app's
//! [`ToolRegistry`], so an agent can extend itself with native plugin tools
//! at runtime — write a plugin crate (file tools), build it (shell), point
//! the registry at it (`add_plugin_path` for scratch dirs) and load it
//! (`reload_plugins`) — without a user-driven restart.
//!
//! A re-scan of an already-loaded plugin reports `skipped` (tool name
//! collision), never an error; a corrupt library reports `failed` with the
//! loader's message. Neither aborts the scan of the remaining files.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::tools::registry::{PluginLoadOutcome, PluginLoadStatus, ToolRegistry};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};

/// Serialize one plugin-file outcome for the tool's JSON output.
fn outcome_json(o: &PluginLoadOutcome) -> serde_json::Value {
    serde_json::json!({
        "plugin": o.path.display().to_string(),
        "status": match o.status {
            PluginLoadStatus::Loaded => "loaded",
            PluginLoadStatus::Skipped => "skipped",
            PluginLoadStatus::Failed => "failed",
        },
        "tool": o.tool_name,
        "error": o.error,
    })
}

// ─── reload_plugins ───────────────────────────────────────────────────────────

/// Re-scans the registry's discovery paths for plugin files and loads any new
/// ones; reports per-file `loaded` / `skipped` / `failed` outcomes.
pub struct ReloadPluginsTool {
    registry: Arc<ToolRegistry>,
}

impl ReloadPluginsTool {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

impl Tool for ReloadPluginsTool {
    fn name(&self) -> &str {
        "reload_plugins"
    }

    fn description(&self) -> &str {
        "Re-scan the plugin discovery paths for plugin files (.dll/.so) and load \
         any that are not registered yet. Reports each plugin file as loaded / \
         skipped (already registered) / failed (with the error). Newly loaded \
         tools become available from the NEXT message (the running turn's tool \
         list is fixed at turn start). Add scratch build dirs first with \
         add_plugin_path."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "reload_plugins".to_string(),
            description: "Re-scan and load plugin tools".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: Vec::new(),
            }),
        }
    }

    fn execute(&self, _params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let outcomes = self
            .registry
            .discover_plugins_detailed()
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        let paths = self.registry.discovery_paths();
        let loaded: Vec<serde_json::Value> = outcomes
            .iter()
            .filter(|o| o.status == PluginLoadStatus::Loaded)
            .map(outcome_json)
            .collect();
        let skipped: Vec<serde_json::Value> = outcomes
            .iter()
            .filter(|o| o.status == PluginLoadStatus::Skipped)
            .map(outcome_json)
            .collect();
        let failed: Vec<serde_json::Value> = outcomes
            .iter()
            .filter(|o| o.status == PluginLoadStatus::Failed)
            .map(outcome_json)
            .collect();
        Ok(ToolOutput::Success(serde_json::json!({
            "status": "ok",
            "discovery_paths": paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "loaded": loaded,
            "skipped": skipped,
            "failed": failed,
            "note": if loaded.is_empty() {
                "No new plugin tools loaded. New tools become available from the next message."
            } else {
                "Loaded plugin tools become available from the next message (this turn's tool \
                 list is fixed)."
            },
        })))
    }
}

// ─── add_plugin_path ──────────────────────────────────────────────────────────

/// Registers an EXTRA discovery path on the live registry (idempotent), so a
/// freshly built plugin can be loaded from a scratch dir without touching the
/// default config-dir plugins path.
pub struct AddPluginPathTool {
    registry: Arc<ToolRegistry>,
}

impl AddPluginPathTool {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

impl Tool for AddPluginPathTool {
    fn name(&self) -> &str {
        "add_plugin_path"
    }

    fn description(&self) -> &str {
        "Add an extra directory to the plugin discovery paths (idempotent). \
         Does NOT load anything by itself — call reload_plugins afterwards to \
         scan and load the plugins in it. The path is NOT persisted; it is \
         lost on restart (the default config-dir plugins path always survives)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = HashMap::new();
        props.insert(
            "path".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Directory to scan for .dll/.so plugin files".to_string(),
                nullable: false,
            },
        );
        ToolSchema {
            name: "add_plugin_path".to_string(),
            description: "Add an extra plugin discovery directory".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["path".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path: String = params
            .get("path")
            .ok_or_else(|| ToolError::InvalidParams("path is required".to_string()))?;
        let path = path.trim().to_string();
        if path.is_empty() {
            return Err(ToolError::InvalidParams(
                "path must not be empty".to_string(),
            ));
        }
        let p = PathBuf::from(&path);
        let added = self.registry.add_discovery_path(p);
        Ok(ToolOutput::Success(serde_json::json!({
            "status": if added { "added" } else { "already_present" },
            "path": path,
            "discovery_paths": self
                .registry
                .discovery_paths()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
            "note": if added {
                "Call reload_plugins to scan and load plugins in this directory."
            } else {
                "This path was already in the discovery set."
            },
        })))
    }
}

/// Register the T3b plugin-management tools on the shared registry.
///
/// Called from the app bootstrap (like [`super::register_agent_tools`] /
/// [`super::register_mcp_tools`]); per-profile visibility is gated through
/// `allowed_tools` like any other tool.
pub fn register_plugin_tools(
    registry: &ToolRegistry,
    registry_arc: Arc<ToolRegistry>,
) -> crate::tools::types::ToolResult<()> {
    use crate::tools::registry::ToolEntry;
    use crate::tools::types::ToolMetadata;

    for (name, description, tool) in [
        (
            "reload_plugins",
            "Re-scan the plugin discovery paths and load any new plugin tools (per-file loaded/skipped/failed report)",
            Arc::new(ReloadPluginsTool::new(registry_arc.clone())) as Arc<dyn Tool>,
        ),
        (
            "add_plugin_path",
            "Add an extra directory to the plugin discovery paths (idempotent; call reload_plugins afterwards)",
            Arc::new(AddPluginPathTool::new(registry_arc)) as Arc<dyn Tool>,
        ),
    ] {
        registry.register(ToolEntry {
            tool,
            metadata: ToolMetadata {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: description.to_string(),
                dependencies: vec![],
            },
            loaded_at: std::time::Instant::now(),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
