use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::tools::lib::{Tool, ToolError, ToolLogger, ToolMetadata, ToolSchema};

/// Internal entry wrapping a loaded tool.
pub struct ToolEntry {
    pub tool: Arc<dyn Tool>,
    pub metadata: ToolMetadata,
    pub loaded_at: Instant,
}

impl Clone for ToolEntry {
    fn clone(&self) -> Self {
        Self {
            tool: self.tool.clone(),
            metadata: self.metadata.clone(),
            loaded_at: self.loaded_at,
        }
    }
}

/// Discovers, registers, and manages tool instances.
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, ToolEntry>>,
    discovery_paths: Vec<PathBuf>,
    logger: Arc<dyn ToolLogger>,
}

impl ToolRegistry {
    pub fn new(discovery_paths: Vec<PathBuf>, logger: Arc<dyn ToolLogger>) -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            discovery_paths,
            logger,
        }
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
        map.insert(name.clone(), entry);
        self.logger.log_tool_call(&name, &Default::default());
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
    pub fn to_tool_definitions(&self) -> Vec<crate::tools::lib::ToolDefinition> {
        self.tools
            .read()
            .unwrap()
            .values()
            .map(|e| {
                let schema = e.tool.parameters_schema();
                crate::tools::lib::ToolDefinition {
                    type_name: "function".to_string(),
                    function: crate::tools::lib::ToolFunctionSpec {
                        name: e.tool.name().to_string(),
                        description: e.tool.description().to_string(),
                        parameters: schema.input_type.unwrap_or(crate::tools::lib::JsonSchema {
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
    /// Returns the number of plugins successfully loaded.
    pub fn discover_plugins(&self) -> Result<usize, ToolError> {
        let loader = crate::tools::dynamic::PluginLoader::new(self.logger.clone());
        let mut loaded = 0;
        for path in &self.discovery_paths {
            if !path.exists() {
                continue;
            }
            let entries = std::fs::read_dir(path).map_err(|e| {
                ToolError::PluginLoad(format!("Cannot read discovery path '{}': {}", path.display(), e))
            })?;
            for entry in entries {
                let entry = entry.map_err(ToolError::Io)?;
                let file_path = entry.path();
                let ext = file_path.extension().and_then(|e| e.to_str());
                if ext != Some("dll") && ext != Some("so") {
                    continue;
                }
                match loader.load(&file_path) {
                    Ok(handle) => {
                        let metadata = handle.metadata().clone();
                        let tool = handle.create_tool()?;
                        self.register(ToolEntry {
                            tool,
                            metadata,
                            loaded_at: Instant::now(),
                        })?;
                        loaded += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            plugin = %file_path.display(),
                            error = %e,
                            "Failed to load plugin"
                        );
                    }
                }
            }
        }
        Ok(loaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::lib::{ToolLogger, TracingToolLogger};

    fn mock_logger() -> Arc<dyn ToolLogger> {
        Arc::new(TracingToolLogger)
    }

    fn make_entry(tool: Arc<dyn Tool>) -> ToolEntry {
        ToolEntry {
            tool,
            metadata: ToolMetadata {
                name: "test_tool".to_string(),
                version: "1.0.0".to_string(),
                description: "A test tool".to_string(),
                dependencies: vec![],
            },
            loaded_at: Instant::now(),
        }
    }

    #[test]
    fn test_register_and_get_tool() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        let tool = Arc::new(CalculationTool::new());
        let name = tool.name().to_string();
        registry.register(make_entry(tool)).unwrap();

        let retrieved = registry.get(&name);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name(), name);
    }

    #[test]
    fn test_register_duplicate_tool_fails() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        let tool = Arc::new(CalculationTool::new());
        registry.register(make_entry(tool.clone())).unwrap();

        let result = registry.register(make_entry(tool));
        assert!(result.is_err());
    }

    #[test]
    fn test_unregister_tool() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        let tool = Arc::new(CalculationTool::new());
        let name = tool.name().to_string();
        registry.register(make_entry(tool)).unwrap();

        let result = registry.unregister(&name);
        assert!(result.is_ok());
        assert!(registry.get(&name).is_none());
    }

    #[test]
    fn test_unregister_missing_tool_fails() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        let result = registry.unregister("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_list_tools_empty() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        assert_eq!(registry.list().len(), 0);
    }

    #[test]
    fn test_list_tools_after_adds() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        registry.register(make_entry(Arc::new(CalculationTool::new()))).unwrap();
        registry.register(make_entry(Arc::new(FileIOTool::new()))).unwrap();

        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn test_list_schemas() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        assert!(registry.list_schemas().is_empty());

        registry.register(make_entry(Arc::new(CalculationTool::new()))).unwrap();
        let schemas = registry.list_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "calculation");
    }

    #[test]
    fn test_to_tool_definitions() {
        let registry = ToolRegistry::new(vec![], mock_logger());
        assert!(registry.to_tool_definitions().is_empty());

        registry.register(make_entry(Arc::new(CalculationTool::new()))).unwrap();
        let definitions = registry.to_tool_definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].function.name, "calculation");
        assert_eq!(definitions[0].type_name, "function");
    }
}
