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
