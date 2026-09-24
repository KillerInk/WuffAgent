use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use crate::tools::dynamic::PluginHandle;
use crate::tools::types::{Tool, ToolError, ToolLogger, ToolMetadata, ToolSchema};

/// Outcome of one plugin file during a discovery scan (T3b). The scan itself
/// never fails on a single bad file — this is how the agent (via the
/// `reload_plugins` tool) sees exactly what happened to each `.dll`/`.so`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginLoadOutcome {
    /// The plugin file that was scanned.
    pub path: PathBuf,
    /// `loaded` (newly registered), `skipped` (tool name already registered),
    /// or `failed`.
    pub status: PluginLoadStatus,
    /// The tool name the plugin registered under (loaded/skipped only).
    pub tool_name: Option<String>,
    /// Error text (failed only).
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginLoadStatus {
    Loaded,
    Skipped,
    Failed,
}

/// Internal entry wrapping a loaded tool.
pub struct ToolEntry {
    pub tool: Arc<dyn Tool>,
    pub metadata: ToolMetadata,
    pub loaded_at: Instant,
    /// Keeps the plugin's DLL mapped for as long as this entry (and every
    /// clone of it) is alive. `None` for built-in / MCP / per-execution tools.
    ///
    /// A plugin tool's vtable lives inside its DLL: if the last
    /// `PluginHandle` were dropped while the tool is still registered,
    /// `FreeLibrary` would unmap the DLL and the next vtable call
    /// (`name()` / `parameters_schema()` / `execute()`) would crash with
    /// STATUS_ACCESS_VIOLATION. Field order matters for drops: `tool` is
    /// declared before `plugin` so the tool's drop glue (code from the DLL)
    /// runs while the DLL is still mapped.
    pub plugin: Option<PluginHandle>,
}
impl Clone for ToolEntry {
    fn clone(&self) -> Self {
        Self {
            tool: self.tool.clone(),
            metadata: self.metadata.clone(),
            loaded_at: self.loaded_at,
            plugin: self.plugin.clone(),
        }
    }
}

/// Discovers, registers, and manages tool instances.
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, ToolEntry>>,
    /// Mutex-guarded because T3b's `add_discovery_path` appends at runtime
    /// (the `reload_plugins` / `add_plugin_path` tools) while discovery scans
    /// read the list.
    discovery_paths: Mutex<Vec<PathBuf>>,
    logger: Arc<dyn ToolLogger>,
}

impl ToolRegistry {
    pub fn new(discovery_paths: Vec<PathBuf>, logger: Arc<dyn ToolLogger>) -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            discovery_paths: Mutex::new(discovery_paths),
            logger,
        }
    }

    /// Add an extra directory to scan for plugin files (T3b). Idempotent: a
    /// path already present is not added again. Returns `true` when the path
    /// was new. Does NOT trigger a scan — call `discover_plugins` when the
    /// plugin should actually load.
    pub fn add_discovery_path(&self, path: PathBuf) -> bool {
        let mut paths = self.discovery_paths.lock().unwrap();
        if paths.iter().any(|p| p == &path) {
            return false;
        }
        paths.push(path);
        true
    }

    /// The current discovery paths (a copy), for display in tool output.
    pub fn discovery_paths(&self) -> Vec<PathBuf> {
        self.discovery_paths.lock().unwrap().clone()
    }

    pub fn register(&self, entry: ToolEntry) -> Result<(), ToolError> {
        let name = entry.tool.name().to_string();
        let mut map = self.tools.write().unwrap();
        if map.contains_key(&name) {
            return Err(ToolError::Validation(format!(
                "Tool '{}' is already registered",
                name
            )));
        }
        let from_plugin = entry.plugin.is_some();
        map.insert(name.clone(), entry);
        tracing::info!(tool = %name, from_plugin, "Tool registered");
        Ok(())
    }

    pub fn unregister(&self, name: &str) -> Result<Arc<dyn Tool>, ToolError> {
        let mut map = self.tools.write().unwrap();
        let entry = map
            .remove(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
        drop(map);
        self.logger.log_plugin_unload(name);
        Ok(entry.tool)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().unwrap().get(name).map(|e| e.tool.clone())
    }

    pub fn list(&self) -> Vec<ToolEntry> {
        self.tools.read().unwrap().values().cloned().collect()
    }

    /// Return tool schemas formatted for AI function-calling.
    pub fn list_schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .read()
            .unwrap()
            .values()
            .map(|e| e.tool.parameters_schema())
            .collect()
    }

    /// Return tool definitions in OpenAI-compatible format.
    pub fn to_tool_definitions(&self) -> Vec<crate::tools::types::ToolDefinition> {
        self.tools
            .read()
            .unwrap()
            .values()
            .map(|e| {
                let schema = e.tool.parameters_schema();
                crate::tools::types::ToolDefinition {
                    type_name: "function".to_string(),
                    function: crate::tools::types::ToolFunctionSpec {
                        name: e.tool.name().to_string(),
                        description: e.tool.description().to_string(),
                        parameters: schema
                            .input_type
                            .unwrap_or(crate::tools::types::JsonSchema {
                                type_name: "object".to_string(),
                                properties: None,
                                required: vec![],
                            }),
                    },
                }
            })
            .collect()
    }

    /// Scan all discovery paths for plugin files (.dll / .so) and load them.
    /// Returns the number of plugins successfully loaded (kept for the
    /// startup call site; use [`Self::discover_plugins_detailed`] to see
    /// per-file outcomes — a bad file never fails the scan, it is reported).
    pub fn discover_plugins(&self) -> Result<usize, ToolError> {
        Ok(self.discover_plugins_detailed()?.iter().filter(|o| o.status == PluginLoadStatus::Loaded).count())
    }

    /// T3b: the same scan, with a per-file outcome for every plugin file seen
    /// (`loaded` / `skipped` = already registered / `failed`). Never returns
    /// `Err` for individual plugin files; `Err` only when a discovery path
    /// itself cannot be read.
    pub fn discover_plugins_detailed(&self) -> Result<Vec<PluginLoadOutcome>, ToolError> {
        let loader = crate::tools::dynamic::PluginLoader::new(self.logger.clone());
        let mut outcomes: Vec<PluginLoadOutcome> = Vec::new();
        tracing::info!(
            paths = ?self.discovery_paths(),
            "Scanning plugin directories"
        );
        for path in self.discovery_paths() {
            if !path.exists() {
                continue;
            }
            let entries = std::fs::read_dir(&path).map_err(|e| {
                ToolError::PluginLoad(format!(
                    "Cannot read discovery path '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            for entry in entries {
                let entry = entry.map_err(ToolError::Io)?;
                let file_path = entry.path();
                let ext = file_path.extension().and_then(|e| e.to_str());
                if ext != Some("dll") && ext != Some("so") {
                    continue;
                }
                match self.load_one_plugin(&loader, &file_path) {
                    Ok(outcome) => outcomes.push(outcome),
                    Err(e) => {
                        tracing::warn!(
                            plugin = %file_path.display(),
                            error = %e,
                            "Failed to load plugin"
                        );
                        outcomes.push(PluginLoadOutcome {
                            path: file_path,
                            status: PluginLoadStatus::Failed,
                            tool_name: None,
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
        }
        let loaded = outcomes
            .iter()
            .filter(|o| o.status == PluginLoadStatus::Loaded)
            .count();
        let skipped = outcomes
            .iter()
            .filter(|o| o.status == PluginLoadStatus::Skipped)
            .count();
        let failed = outcomes
            .iter()
            .filter(|o| o.status == PluginLoadStatus::Failed)
            .count();
        tracing::info!(
            files = outcomes.len(),
            loaded,
            skipped,
            failed,
            "Plugin scan complete"
        );
        Ok(outcomes)
    }

    /// Load + register a single plugin file, classifying the outcome.
    /// `Err` means the library itself could not be loaded or the tool could
    /// not be created (a `failed` outcome); a tool-name collision is a
    /// `skipped` outcome (the tool is already available under that name).
    fn load_one_plugin(
        &self,
        loader: &crate::tools::dynamic::PluginLoader,
        file_path: &Path,
    ) -> Result<PluginLoadOutcome, ToolError> {
        tracing::debug!(path = %file_path.display(), "Loading plugin file");
        let handle = loader.load(file_path)?;
        let metadata = handle.metadata().clone();
        let tool = handle.create_tool()?;
        let name = tool.name().to_string();
        match self.register(ToolEntry {
            tool,
            metadata,
            loaded_at: Instant::now(),
            // Keep the DLL mapped for the tool's whole registered lifetime
            // (the entry's vtable lives inside the DLL — see `plugin`).
            plugin: Some(handle),
        }) {
            Ok(()) => {
                tracing::info!(
                    tool = %name,
                    path = %file_path.display(),
                    "Plugin tool registered (DLL kept alive by the entry)"
                );
                Ok(PluginLoadOutcome {
                    path: file_path.to_path_buf(),
                    status: PluginLoadStatus::Loaded,
                    tool_name: Some(name),
                    error: None,
                })
            }
            // Already registered (re-scan of an already-loaded plugin): the
            // tool is available, so this is a skip, not a failure.
            // (The freshly created handle/tool are dropped here; the earlier
            // entry's keepalive still maps the DLL.)
            Err(ToolError::Validation(msg)) if msg.contains("already registered") => {
                tracing::info!(tool = %name, "Plugin tool skipped (already registered)");
                Ok(PluginLoadOutcome {
                    path: file_path.to_path_buf(),
                    status: PluginLoadStatus::Skipped,
                    tool_name: Some(name),
                    error: None,
                })
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests;
