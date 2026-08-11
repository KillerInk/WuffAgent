use std::path::Path;
use std::sync::Arc;

use libloading::{Library, Symbol};

use crate::tools::lib::{PluginTool, Tool, ToolError, ToolLogger, ToolMetadata, ToolResult};

/// A handle to a dynamically loaded plugin.
pub struct PluginHandle {
    lib: Library,
    metadata: ToolMetadata,
}

/// Signature of the plugin's entry point.
// Plugin ABI: plugin exports this symbol to create a new tool instance.
// The caller is responsible for managing the resulting Arc<dyn Tool>.
// Plugins should use PluginTool::from_box() to wrap their Tool before returning.
type PluginCreateFn = unsafe extern "C" fn() -> PluginTool;

impl PluginHandle {
    /// Load a plugin from the given path.
    pub fn load(path: &Path, logger: Arc<dyn ToolLogger>) -> ToolResult<Self> {
        // SAFETY: libloading and raw pointer operations below are all safe in context.
        unsafe {
            let lib = Library::new(path).map_err(|e| {
                ToolError::PluginLoad(format!(
                    "Failed to load '{}': {}",
                    path.display(),
                    e
                ))
            })?;

            // Try to read the metadata symbol
            let metadata_ptr: Symbol<unsafe extern "C" fn() -> *const ToolMetadata> = lib
                .get(b"wuff_tool_metadata")
                .map_err(|e| {
                    ToolError::PluginLoad(format!("Missing metadata symbol: {}", e))
                })?;
            let metadata_ref = &*metadata_ptr();
            let metadata = metadata_ref.clone();

            if metadata.name.is_empty() {
                return Err(ToolError::PluginLoad(
                    "Plugin metadata has empty name".to_string(),
                ));
            }

            logger.log_plugin_load(path, &metadata);

            Ok(Self { lib, metadata })
        }
    }

    /// Create a new tool instance from this plugin.
    pub fn create_tool(&self) -> ToolResult<Arc<dyn Tool>> {
        unsafe {
            let create_fn: Symbol<PluginCreateFn> = self
                .lib
                .get(b"wuff_tool_create")
                .map_err(|e| {
                    ToolError::PluginLoad(format!("Missing create symbol: {}", e))
                })?;
            let raw = create_fn();
            // SAFETY: The plugin is responsible for returning a valid PluginTool
            // created via PluginTool::from_box(). We take ownership and convert.
            let tool: Box<dyn Tool> = raw.into_box();
            let arc = Arc::from_raw(Box::into_raw(tool));
            Ok(arc)
        }
    }

    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }
}
