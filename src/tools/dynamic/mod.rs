pub mod loader;
pub use loader::PluginHandle;

use std::sync::Arc;

pub use super::lib::{ToolLogger, ToolResult};

/// A loader that can be shared across the registry.
pub struct PluginLoader {
    logger: Arc<dyn ToolLogger>,
}

impl PluginLoader {
    pub fn new(logger: Arc<dyn ToolLogger>) -> Self {
        Self { logger }
    }

    /// Load a plugin from the given path.
    pub fn load(&self, path: &std::path::Path) -> ToolResult<PluginHandle> {
        PluginHandle::load(path, self.logger.clone())
    }
}
