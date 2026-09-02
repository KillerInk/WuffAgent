pub mod calculation;
pub mod file_io;
pub mod web_search;
pub mod agent_call;
pub mod time;
pub mod shell;

pub use calculation::CalculationTool;
pub use file_io::{
    AppendFileTool, ApplyDiffTool, FileIOTool, FileOpsTool, ListDirTool, ReadFileTool,
    SearchFilesTool, WriteFileTool,
};
pub use web_search::WebSearchTool;
pub use agent_call::AgentCallTool;
pub use time::TimeTool;
pub use shell::{ShellTool, ShellConfig};

use crate::tools::types::{Tool, ToolMetadata};
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

    // Named file tools (split from the old 14-action file_io god-tool so the
    // model routes by clear tool names instead of an `action` string).
    for (name, desc, tool) in [
        (
            "read_file",
            "Read a text file, optionally limited to a line range",
            std::sync::Arc::new(ReadFileTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "write_file",
            "Write (overwrite) a text file with the given content",
            std::sync::Arc::new(WriteFileTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "append_file",
            "Append content to the end of a file",
            std::sync::Arc::new(AppendFileTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "list_dir",
            "List the entries in a directory",
            std::sync::Arc::new(ListDirTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "search_files",
            "Find files matching a glob pattern",
            std::sync::Arc::new(SearchFilesTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "apply_diff",
            "Apply a unified diff/patch to an existing file",
            std::sync::Arc::new(ApplyDiffTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "file_ops",
            "File operations: copy, move, rename, mkdir, delete, read_binary, write_binary, file_info",
            std::sync::Arc::new(FileOpsTool::new()) as std::sync::Arc<dyn Tool>,
        ),
    ] {
        registry.register(ToolEntry {
            tool,
            metadata: ToolMetadata {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: desc.to_string(),
                dependencies: vec![],
            },
            loaded_at: std::time::Instant::now(),
        })?;
    }

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