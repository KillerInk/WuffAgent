pub mod calculation;
pub mod file_io;
pub mod web_search;
pub mod agent_call;
pub mod time;
pub mod shell;

pub use calculation::CalculationTool;
pub use file_io::FileIOTool;
pub use web_search::WebSearchTool;
pub use agent_call::AgentCallTool;
pub use time::TimeTool;
pub use shell::{ShellTool, ShellConfig};

use crate::tools::types::{ToolMetadata};
use crate::tools::registry::ToolEntry;
/// Register all built-in tools into the registry.
pub fn register_builtins(
    registry: &crate::tools::registry::ToolRegistry,
    invocation_registry: &crate::agents::invocation_registry::AgentInvocationRegistry,
) -> crate::tools::types::ToolResult<()> {
    // Create web search tool
    registry.register(ToolEntry {
        tool: std::sync::Arc::new(WebSearchTool::new()),
        metadata: ToolMetadata {
            name: "web_search".to_string(),
            version: "1.0.0".to_string(),
            description: "Search the web for information using a search engine".to_string(),
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
            description: "Evaluate mathematical expressions. Supports basic arithmetic (+, -, *, /), exponentiation (^), parentheses, negative numbers, common functions (sin, cos, tan, asin, acos, atan, sqrt, log, ln, log2, log10, abs, floor, ceil, exp, round, fact), and constants (pi, e)".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    registry.register(ToolEntry {
        tool: std::sync::Arc::new(AgentCallTool::new(std::sync::Arc::new(invocation_registry.clone()))),
        metadata: ToolMetadata {
            name: "agent_call".to_string(),
            version: "1.0.0".to_string(),
            description: "Invoke another agent to execute a sub-task".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    registry.register(ToolEntry {
        tool: std::sync::Arc::new(TimeTool::new()),
        metadata: ToolMetadata {
            name: "time".to_string(),
            version: "1.0.0".to_string(),
            description: "Get the current date and time".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    registry.register(ToolEntry {
        tool: std::sync::Arc::new(ShellTool::new(ShellConfig {
            allowed_commands: Vec::new(),
            shell_type: "powershell".to_string(),
            timeout_ms: 300_000,
            enabled: true,
            working_dir: None,
        })),
        metadata: ToolMetadata {
            name: "shell".to_string(),
            version: "1.0.0".to_string(),
            description: "Execute shell commands on the local system".to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
    })?;

    Ok(())
}