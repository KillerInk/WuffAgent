pub mod agent_profile;
pub mod calculation;
pub mod fetch_url;
pub mod fileio;
pub mod image;
pub mod hand_back;
pub mod handoff;
pub mod improvement;
pub(crate) mod html;
pub mod memory;
pub mod mcp;
pub mod plugins;
pub mod restart;
pub mod search;
pub mod shell;
pub mod skills;
pub mod time;
pub mod web_search;

pub use agent_profile::{EditAgentProfileTool, ListAgentsTool};
pub use calculation::CalculationTool;
pub use fetch_url::FetchUrlTool;
pub use image::ShowImageTool;
pub use fileio::{
    AppendFileTool, ApplyDiffTool, CopyTool, DeleteTool, FileInfoTool, ListDirTool, MkdirTool,
    MoveTool, ReadFileTool, SearchFilesTool, WriteFileTool,
};
pub use memory::{
    ConsolidateMemoriesTool, DeleteMemoryTool, SaveMemoryTool, SearchMemoryTool, UpdateMemoryTool,
};
pub use mcp::{
    McpAddServerTool, McpConnectTool, McpDisconnectTool, McpListTool, McpRemoveServerTool,
    McpRefreshToolsTool, McpSetToolEnabledTool,
};
pub use plugins::{AddPluginPathTool, ReloadPluginsTool};
pub use search::SearchContentTool;
pub use shell::{ShellConfig, ShellTool};
pub use skills::{DeleteSkillTool, ListSkillsTool, ReadSkillTool, SaveSkillTool};
pub use time::TimeTool;
pub use web_search::WebSearchTool;
// NOTE: HandoffTool is NOT registered in `register_builtins` — it is
// per-execution (own mailbox / agents dir / allowlist) and is injected by
// `Agent::builder` for agents with `handoff_enabled`, like the per-agent shell.
pub use improvement::{
    register_improvement_tools, ListImprovementStatusTool, ReadMetricsTool,
    RunSelfImprovementTool,
};
pub use handoff::HandoffTool;
// NOTE: RestartTool is also per-execution (own mailbox) — injected by
// `Agent::builder` for agents with `restart_enabled`, not registered in
// `register_builtins`.
pub use restart::RestartTool;
// NOTE: HandBackTool is also per-execution (own mailbox) — injected by
// `Agent::builder` for sub-session agents (`hand_back_enabled` + a session meta
// with a `parent_session_id`), not registered in `register_builtins`.
pub use hand_back::HandBackTool;

use crate::tools::registry::{ToolEntry, ToolRegistry};
use crate::tools::types::{Tool, ToolMetadata};

/// Register one tool with the standard builtin entry shape (version 1.0.0,
/// no dependencies, no owning plugin). The single place the `ToolEntry`
/// construction for shared-registry tools happens.
fn register_tool(
    registry: &ToolRegistry,
    name: &str,
    description: &str,
    tool: std::sync::Arc<dyn Tool>,
) -> crate::tools::types::ToolResult<()> {
    registry.register(ToolEntry {
        tool,
        metadata: ToolMetadata {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: description.to_string(),
            dependencies: vec![],
        },
        loaded_at: std::time::Instant::now(),
        plugin: None,
    })
}

/// Register all built-in tools that do not require external dependencies.
pub fn register_builtins(
    registry: &ToolRegistry,
    search: &crate::config::SearchConfig,
) -> crate::tools::types::ToolResult<()> {
    // Create web search tool (backend + limits come from the search config,
    // with env-var overrides applied for headless use).
    register_tool(
        registry,
        "web_search",
        "Search the web (Bing/Yahoo/DuckDuckGo with automatic failover, or SearXNG)",
        std::sync::Arc::new(
            WebSearchTool::new()
                .with_backend(crate::config::SearchBackend::resolve_with_env(search))
                .with_default_max_results(search.max_results)
                .with_cache_ttl(std::time::Duration::from_secs(search.cache_duration_secs)),
        ),
    )?;

    // Fetch a web page as plain text (companion to web_search).
    register_tool(
        registry,
        "fetch_url",
        "Fetch a URL and return its content as plain text (HTML is converted to text). Params: url (required), max_bytes (optional, default 128KB)",
        std::sync::Arc::new(FetchUrlTool::new()),
    )?;

    // Load + display an image (file path, data: URI, or URL) in the chat.
    register_tool(
        registry,
        "show_image",
        "Load an image (file path, data: URI, or http(s) URL), downscale it, and display it in the chat (the UI renders the returned data URI in the tool card)",
        std::sync::Arc::new(ShowImageTool::new()),
    )?;

    // Named file tools (split from the old 14-action file_io god-tool so the
    // model routes by clear tool names instead of an `action` string).
    for (name, desc, tool) in [
        (
            "read_file",
            "Read a text file, optionally limited to a line range. Large files are truncated with a `truncated` flag and `total_lines` for paging",
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
            "List directory entries (name, type, size) with directories first",
            std::sync::Arc::new(ListDirTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "search_files",
            "Find files matching a glob pattern. Skips .git and target directories; capped at 500 matches",
            std::sync::Arc::new(SearchFilesTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "apply_diff",
            "Apply targeted edits to an existing file using SEARCH/REPLACE blocks",
            std::sync::Arc::new(ApplyDiffTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "mkdir",
            "Create a directory (creates parent directories when recursive is true)",
            std::sync::Arc::new(MkdirTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "delete",
            "Delete a file or directory (non-empty directories require recursive)",
            std::sync::Arc::new(DeleteTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "copy",
            "Copy a file or directory (directories are copied recursively)",
            std::sync::Arc::new(CopyTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "move",
            "Move or rename a file or directory",
            std::sync::Arc::new(MoveTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "file_info",
            "Get file or directory metadata: size, modified time, type, permissions",
            std::sync::Arc::new(FileInfoTool::new()) as std::sync::Arc<dyn Tool>,
        ),
        (
            "search_content",
            "Search for a text pattern or regex in file contents (like grep/ripgrep)",
            std::sync::Arc::new(SearchContentTool::new()) as std::sync::Arc<dyn Tool>,
        ),
    ] {
        register_tool(registry, name, desc, tool)?;
    }

    register_tool(
        registry,
        "calculation",
        "Evaluate mathematical expressions. Supports basic arithmetic (+, -, *, /), exponentiation (^), parentheses, negative numbers, common functions (sin, cos, tan, asin, acos, atan, sqrt, log, ln, log2, log10, abs, floor, ceil, exp, round, fact), and constants (pi, e)",
        std::sync::Arc::new(CalculationTool::new()),
    )?;

    register_tool(
        registry,
        "time",
        "Get the current date and time",
        std::sync::Arc::new(TimeTool::new()),
    )?;

    register_tool(
        registry,
        "shell",
        "Execute shell commands on the local system",
        std::sync::Arc::new(ShellTool::new(ShellConfig {
            allowed_commands: Vec::new(),
            shell_type: "powershell".to_string(),
            timeout_ms: 300_000,
            enabled: true,
            working_dir: None,
        })),
    )?;

    Ok(())
}

/// Register the agent-profile self-modification tools (T1): `list_agents`
/// and `edit_agent_profile`.
///
/// Shared-registry tools (not per-execution): they need no per-run state, so
/// they are registered once with an [`AgentManager`] bound to the same
/// discovery set the UI agent selector uses (primary dir + search dirs).
/// Visibility is gated per profile through `allowed_tools` like any other
/// tool; removing the tools from a profile requires `allow_self_removal`.
pub fn register_agent_tools(
    registry: &ToolRegistry,
    agent_manager: std::sync::Arc<crate::agents::manager::AgentManager>,
) -> crate::tools::types::ToolResult<()> {
    register_tool(
        registry,
        "list_agents",
        "List all agent profiles (name, description, enabled, path, allowed_tools, handoff/restart settings, prompt preview)",
        std::sync::Arc::new(ListAgentsTool::new(agent_manager.clone())),
    )?;

    register_tool(
        registry,
        "edit_agent_profile",
        "Edit an agent profile by name (fields omitted are left unchanged; pre-edit snapshot, rename via new_name)",
        std::sync::Arc::new(EditAgentProfileTool::new(agent_manager)),
    )?;

    Ok(())
}

/// Register the MCP management tools (T2): `mcp_list`, `mcp_add_server`,
/// `mcp_connect`, `mcp_disconnect`, `mcp_remove_server`, `mcp_refresh_tools`,
/// `mcp_set_tool_enabled`.
///
/// Shared-registry tools (not per-execution): they wrap the app's
/// [`McpManager`] (which already owns its dedicated runtime — the `*_sync`
/// ops run on a worker thread inside each tool call, so the tool thread never
/// blocks a runtime context). Visibility is gated per profile through
/// `allowed_tools` like any other tool.
pub fn register_mcp_tools(
    registry: &ToolRegistry,
    mcp: std::sync::Arc<crate::tools::mcp::McpManager>,
    event_tx: Option<std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Sender<crate::types::AppEvent>>>>,
    session_id: Option<String>,
) -> crate::tools::types::ToolResult<()> {
    let add = match &event_tx {
        Some(tx) => McpAddServerTool::new(mcp.clone()).with_events(tx.clone(), session_id.clone()),
        None => McpAddServerTool::new(mcp.clone()),
    };
    let remove = match &event_tx {
        Some(tx) => McpRemoveServerTool::new(mcp.clone()).with_events(tx.clone(), session_id.clone()),
        None => McpRemoveServerTool::new(mcp.clone()),
    };
    let tools: Vec<(&str, &str, std::sync::Arc<dyn Tool>)> = vec![
        (
            "mcp_list",
            "List configured MCP servers with live status, transport and their tools (registry names mcp__<server>__<tool>)",
            std::sync::Arc::new(McpListTool::new(mcp.clone())),
        ),
        (
            "mcp_add_server",
            "Add or replace an MCP server (stdio command/args/env or http url/headers); persists to config.json and connects when enabled",
            std::sync::Arc::new(add),
        ),
        (
            "mcp_connect",
            "Connect an MCP server (handshake + tools/list + register enabled tools)",
            std::sync::Arc::new(McpConnectTool::new(mcp.clone())),
        ),
        (
            "mcp_disconnect",
            "Disconnect an MCP server (unregister its tools, kill its process)",
            std::sync::Arc::new(McpDisconnectTool::new(mcp.clone())),
        ),
        (
            "mcp_remove_server",
            "Remove an MCP server from the live state and from config.json",
            std::sync::Arc::new(remove),
        ),
        (
            "mcp_refresh_tools",
            "Re-list a connected MCP server's tools and re-register the enabled ones",
            std::sync::Arc::new(McpRefreshToolsTool::new(mcp.clone())),
        ),
        (
            "mcp_set_tool_enabled",
            "Enable or disable one MCP server tool (registers/unregisters mcp__<server>__<tool>)",
            std::sync::Arc::new(McpSetToolEnabledTool::new(mcp)),
        ),
    ];
    for (name, description, tool) in tools {
        register_tool(registry, name, description, tool)?;
    }
    Ok(())
}

/// Register the T3b plugin-management tools: `reload_plugins` and
/// `add_plugin_path`.
///
/// Shared-registry tools wrapping the app's [`ToolRegistry`] itself (the
/// agent can load native plugin tools into the very registry it reads from —
/// the new tools become visible from the next message). Per-profile
/// visibility is gated through `allowed_tools` like any other tool.
pub fn register_plugin_tools(
    registry: &ToolRegistry,
    registry_arc: std::sync::Arc<ToolRegistry>,
) -> crate::tools::types::ToolResult<()> {
    plugins::register_plugin_tools(registry, registry_arc)
}

/// Register memory tools that require a memory manager instance.
/// Call this after creating the memory manager and before using agents.
pub fn register_memory_tools(
    registry: &ToolRegistry,
    memory: std::sync::Arc<crate::memory::MemoryManager>,
) -> crate::tools::types::ToolResult<()> {
    for (name, desc, tool) in [
        (
            "save_memory",
            "Save a persistent fact, lesson, or decision to project memory",
            std::sync::Arc::new(SaveMemoryTool::new(memory.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "update_memory",
            "Update an existing memory entry when information has changed",
            std::sync::Arc::new(UpdateMemoryTool::new(memory.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "search_memory",
            "Search project memory for relevant information",
            std::sync::Arc::new(SearchMemoryTool::new(memory.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "consolidate_memories",
            "Merge multiple related memories into a single comprehensive entry",
            std::sync::Arc::new(ConsolidateMemoriesTool::new(memory.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "delete_memory",
            "Delete a memory entry by ID",
            std::sync::Arc::new(DeleteMemoryTool::new(memory)) as std::sync::Arc<dyn Tool>,
        ),
    ] {
        register_tool(registry, name, desc, tool)?;
    }
    Ok(())
}

/// Register the K1 skill tools: `save_skill`, `list_skills`, `read_skill`,
/// `delete_skill` — backed by a [`SkillStore`](crate::memory::skills::SkillStore)
/// (default root: `<wuffagent_home>/skills`).
///
/// Shared-registry tools; per-profile visibility is gated by `allowed_tools`
/// like any other tool.
pub fn register_skill_tools(
    registry: &ToolRegistry,
    skills: std::sync::Arc<crate::memory::skills::SkillStore>,
) -> crate::tools::types::ToolResult<()> {
    for (name, desc, tool) in [
        (
            "save_skill",
            "Save a reusable procedure (skill) as a named markdown document",
            std::sync::Arc::new(SaveSkillTool::new(skills.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "list_skills",
            "List saved skills (name, description, when_to_use)",
            std::sync::Arc::new(ListSkillsTool::new(skills.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "read_skill",
            "Read a saved skill's full content by name",
            std::sync::Arc::new(ReadSkillTool::new(skills.clone())) as std::sync::Arc<dyn Tool>,
        ),
        (
            "delete_skill",
            "Delete a skill by name",
            std::sync::Arc::new(DeleteSkillTool::new(skills)) as std::sync::Arc<dyn Tool>,
        ),
    ] {
        register_tool(registry, name, desc, tool)?;
    }
    Ok(())
}
