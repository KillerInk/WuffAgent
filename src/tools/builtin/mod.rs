pub mod calculation;
pub mod file_io;
pub mod web_search;

pub use calculation::CalculationTool;
pub use file_io::FileIOTool;
pub use web_search::WebSearchTool;

use crate::tools::lib::{ToolMetadata};
use crate::tools::registry::ToolEntry;

/// Register all built-in tools into the registry.
pub fn register_builtins(registry: &crate::tools::registry::ToolRegistry) -> crate::tools::lib::ToolResult<()> {
    registry.register(ToolEntry {
        tool: std::sync::Arc::new(WebSearchTool::new()),
        metadata: ToolMetadata {
            name: "web_search".to_string(),
            version: "1.0.0".to_string(),
            description: "Search the web for information".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    registry.register(ToolEntry {
        tool: std::sync::Arc::new(FileIOTool::new()),
        metadata: ToolMetadata {
            name: "file_io".to_string(),
            version: "1.0.0".to_string(),
            description: "Read, write, and list files on the local system".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    registry.register(ToolEntry {
        tool: std::sync::Arc::new(CalculationTool::new()),
        metadata: ToolMetadata {
            name: "calculation".to_string(),
            version: "1.0.0".to_string(),
            description: "Perform mathematical calculations".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    Ok(())
}
