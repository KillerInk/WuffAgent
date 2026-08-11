use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ─── Error Types ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    NotFound(String),
    #[error("Tool execution failed: {0}")]
    Execution(String),
    #[error("Invalid parameters: {0}")]
    InvalidParams(String),
    #[error("Plugin load error: {0}")]
    PluginLoad(String),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type ToolResult<T> = Result<T, ToolError>;

// ─── Schema Types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub type_name: String,
    pub description: String,
    pub nullable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, FieldSchema>>,
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<JsonSchema>,
}

// ─── Tool Definition (for AI function calling) ─────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(rename = "function")]
    pub function: ToolFunctionSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolFunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchema,
}

// ─── Tool Parameters and Output ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolParams {
    pub values: HashMap<String, serde_json::Value>,
}

impl ToolParams {
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.values.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

impl Default for ToolParams {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ToolOutput {
    Success(serde_json::Value),
    Error(String),
}

impl ToolOutput {
    pub fn success(value: impl Into<serde_json::Value>) -> Self {
        ToolOutput::Success(value.into())
    }

    pub fn error(msg: impl Into<String>) -> Self {
        ToolOutput::Error(msg.into())
    }
}

// ─── Tool Metadata ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum ToolSource {
    BuiltIn,
    Dynamic(std::path::PathBuf),
}

// ─── Core Tool Trait ────────────────────────────────────────────────────────

pub trait Tool: Send + Sync {
    /// Human-readable name of the tool.
    fn name(&self) -> &str;

    /// Description used for AI function-calling schemas.
    fn description(&self) -> &str;

    /// JSON schema describing the expected input parameters.
    fn parameters_schema(&self) -> ToolSchema;

    /// Execute the tool with the given parameters.
    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput>;
}

// ─── FFI Plugin ABI ─────────────────────────────────────────────────────────

/// Opaque FFI-safe wrapper for passing trait objects across the plugin boundary.
/// Plugins box their Tool and return this wrapper; the host converts it back.
/// Uses two raw pointers (vtable + data) to represent the wide *mut dyn Tool pointer.
#[repr(C)]
pub struct PluginTool {
    vtable: *const (),
    data: *mut std::ffi::c_void,
}

// SAFETY: PluginTool only stores pointers and is passed by value across FFI.
// The host is responsible for ensuring the underlying Tool lives long enough.
unsafe impl Send for PluginTool {}
// SAFETY: Tool requires Send + Sync, and PluginTool merely wraps a Tool pointer.
unsafe impl Sync for PluginTool {}

impl PluginTool {
    /// Create a new PluginTool from a Box<dyn Tool>.
    pub fn from_box(tool: Box<dyn Tool>) -> Self {
        // SAFETY: We split the wide pointer into its vtable and data components.
        // Box::into_raw gives us *mut dyn Tool (128 bits on 64-bit).
        // We use ptr::from_mut to get the raw components.
        let raw: *mut dyn Tool = Box::into_raw(tool);
        // SAFETY: A wide pointer is exactly two pointers (vtable, data) in memory.
        let (vtable, data) = unsafe {
            let slice: [*const (); 2] = std::mem::transmute(raw);
            (slice[0], slice[1] as *mut std::ffi::c_void)
        };
        Self { vtable, data }
    }

    /// Convert this wrapper back into an owned Box<dyn Tool>.
    /// # Safety
    /// The caller must ensure the PluginTool was created via `from_box`
    /// and has not been consumed yet.
    pub unsafe fn into_box(self) -> Box<dyn Tool> {
        // SAFETY: We reconstruct the wide pointer from its vtable and data components.
        let raw: *mut dyn Tool = std::mem::transmute([self.vtable, self.data as *const ()]);
        Box::from_raw(raw)
    }
}

impl Drop for PluginTool {
    fn drop(&mut self) {
        // SAFETY: We own the pointer and are dropping it here.
        unsafe {
            let raw: *mut dyn Tool = std::mem::transmute([self.vtable, self.data as *const ()]);
            drop(Box::from_raw(raw));
        }
    }
}

// ─── Logger Trait ───────────────────────────────────────────────────────────

pub trait ToolLogger: Send + Sync {
    fn log_tool_call(&self, tool_name: &str, params: &ToolParams);
    fn log_tool_result(&self, tool_name: &str, result: &ToolOutput);
    fn log_tool_error(&self, tool_name: &str, error: &ToolError);
    fn log_plugin_load(&self, path: &std::path::Path, metadata: &ToolMetadata);
    fn log_plugin_unload(&self, tool_name: &str);
}

/// Default logger backed by `tracing`.
#[derive(Clone)]
pub struct TracingToolLogger;

impl ToolLogger for TracingToolLogger {
    fn log_tool_call(&self, tool_name: &str, params: &ToolParams) {
        tracing::info!(tool = tool_name, params = ?params, "Tool called");
    }

    fn log_tool_result(&self, tool_name: &str, result: &ToolOutput) {
        match result {
            ToolOutput::Success(_) => {
                tracing::info!(tool = tool_name, "Tool succeeded");
            }
            ToolOutput::Error(msg) => {
                tracing::warn!(tool = tool_name, error = %msg, "Tool returned error output");
            }
        }
    }

    fn log_tool_error(&self, tool_name: &str, error: &ToolError) {
        tracing::error!(tool = tool_name, error = %error, "Tool execution failed");
    }

    fn log_plugin_load(&self, path: &std::path::Path, metadata: &ToolMetadata) {
        tracing::info!(
            plugin = %path.display(),
            name = metadata.name,
            version = metadata.version,
            "Plugin loaded"
        );
    }

    fn log_plugin_unload(&self, tool_name: &str) {
        tracing::info!(tool = tool_name, "Plugin unloaded");
    }
}

impl fmt::Display for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolOutput::Success(v) => write!(f, "{}", v),
            ToolOutput::Error(e) => write!(f, "ERROR: {}", e),
        }
    }
}
