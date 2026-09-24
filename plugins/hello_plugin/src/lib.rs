//! Minimal WuffAgent plugin — the `hello` tool.
//!
//! Reference implementation of the WuffAgent plugin ABI.
//!
//! A WuffAgent plugin is a `cdylib` that exports three C symbols:
//!
//!   * `wuff_tool_abi_version` -> `u32` (MUST equal
//!     `wuffagent_core::tools::types::PLUGIN_ABI_VERSION` — the loader
//!     rejects the plugin otherwise, so a stale DLL fails cleanly instead of
//!     corrupting the host's heap)
//!   * `wuff_tool_metadata` -> `*const ToolMetadata` (a leaked Box; the loader
//!     clones the pointee and keeps the pointer for the process lifetime)
//!   * `wuff_tool_create`   -> `PluginTool` (the tool instance, wrapped via
//!     `PluginTool::from_box`)
//!
//! Build (debug, from the repo root):
//!
//! ```sh
//! cargo build -p hello_plugin
//! # Windows:  target/debug/hello_plugin.dll
//! # macOS:    target/debug/libhello_plugin.dylib   (rename to .so to be scanned)
//! # Linux:    target/debug/libhello_plugin.so
//! ```
//!
//! Install: copy the `.dll`/`.so` into the plugins dir
//! (`~/.wuffagent/plugins/` — the plugins subdir of WuffAgent's config
//! directory, next to `agents/` and `sessions/`), then in WuffAgent call the
//! `reload_plugins` tool (or restart the app). The `hello` tool is available
//! to any agent whose profile does not restrict it out of `allowed_tools`
//! (empty `allowed_tools` = all tools).

use wuffagent_core::tools::types::{
    FieldSchema, JsonSchema, PluginTool, Tool, ToolMetadata, ToolOutput, ToolParams, ToolResult,
    ToolSchema,
};

/// The plugin's tool: echoes the `message` argument back with a prefix.
pub struct HelloTool;

impl Tool for HelloTool {
    fn name(&self) -> &str {
        "hello"
    }

    fn description(&self) -> &str {
        "Say hello (the plugin tool). Params: message (optional, default 'world')"
    }

    fn parameters_schema(&self) -> ToolSchema {
        let mut props = std::collections::HashMap::new();
        props.insert(
            "message".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Who to greet".to_string(),
                nullable: true,
            },
        );
        ToolSchema {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: Vec::new(),
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let who = params
            .get::<String>("message")
            .unwrap_or_else(|| "world".to_string());
        Ok(ToolOutput::Success(serde_json::json!({
            "greeting": format!("hello, {}!", who),
            "from": "hello_plugin",
        })))
    }
}

/// `wuff_tool_abi_version` — the plugin ABI version this plugin was compiled
/// against. The loader compares it with
/// `wuffagent_core::tools::types::PLUGIN_ABI_VERSION` and rejects the plugin
/// on mismatch (see that constant for why a stale DLL is dangerous).
#[no_mangle]
pub extern "C" fn wuff_tool_abi_version() -> u32 {
    wuffagent_core::tools::types::PLUGIN_ABI_VERSION
}

/// `wuff_tool_metadata` — the loader reads `*const ToolMetadata` here and
/// clones the metadata. `Box::leak` keeps it alive for the process lifetime
/// (plugins are loaded once and live until process exit).
///
/// SAFETY: the returned pointer is valid for the process lifetime and
/// never freed (leaked box).
#[no_mangle]
pub extern "C" fn wuff_tool_metadata() -> *const ToolMetadata {
    let box_meta: Box<ToolMetadata> = Box::new(ToolMetadata {
        name: "hello".to_string(),
        version: "0.1.0".to_string(),
        description: "Say hello (the plugin tool)".to_string(),
        dependencies: vec![],
    });
    Box::leak(box_meta)
}

/// `wuff_tool_create` — the loader calls this to get a new tool instance
/// (each registry entry gets its own instance).
///
/// SAFETY: returns a valid `PluginTool` built from a `Box<dyn Tool>`, as
/// documented on `PluginTool::from_box`.
#[no_mangle]
pub extern "C" fn wuff_tool_create() -> PluginTool {
    PluginTool::from_box(Box::new(HelloTool) as Box<dyn Tool>)
}
