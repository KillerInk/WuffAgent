use std::collections::HashMap;
use std::fs;

use crate::tools::lib::{Tool, ToolOutput, ToolParams, ToolSchema};

/// A tool that performs read/write/list operations on the local filesystem.
pub struct FileIOTool;

/// Validates a path for safety, rejecting dangerous paths and path traversal patterns.
fn validate_path(path: &str) -> Result<(), crate::tools::lib::ToolError> {
    // Reject absolute paths to sensitive system directories
    if path == "/etc" || path.starts_with("/etc/") || path == "/root" || path.starts_with("/root/") {
        return Err(crate::tools::lib::ToolError::Execution("Path not allowed".to_string()));
    }
    // Reject Windows system directories
    let lower = path.to_lowercase();
    if lower == "c:\\windows" || lower.starts_with("c:\\windows\\") {
        return Err(crate::tools::lib::ToolError::Execution("Path not allowed".to_string()));
    }
    // Reject path traversal patterns
    if path.contains("..") {
        return Err(crate::tools::lib::ToolError::Execution("Path not allowed".to_string()));
    }
    Ok(())
}

impl FileIOTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileIOTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for FileIOTool {
    fn name(&self) -> &str {
        "file_io"
    }

    fn description(&self) -> &str {
        "Read, write, and list files on the local system"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_io".to_string(),
            description: "Read, write, or list files on the local system".to_string(),
            input_type: Some(crate::tools::lib::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "path".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "File or directory path".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "content".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Content to write (for write action)".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "action".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Action to perform: 'read', 'write', or 'list'".to_string(),
                            nullable: false,
                        },
                    );
                    map
                }),
                required: vec!["path".to_string(), "action".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::lib::ToolResult<ToolOutput> {
        let path: String = params
            .get("path")
            .ok_or_else(|| crate::tools::lib::ToolError::InvalidParams("path is required".to_string()))?;
        let action: String = params
            .get("action")
            .ok_or_else(|| crate::tools::lib::ToolError::InvalidParams("action is required".to_string()))?;

        validate_path(&path)?;

        match action.as_str() {
            "read" => {
                let content = fs::read_to_string(&path).map_err(|e| {
                    crate::tools::lib::ToolError::Execution(format!("Failed to read '{}': {}", path, e))
                })?;
                Ok(ToolOutput::Success(serde_json::json!({
                    "path": path,
                    "content": content
                })))
            }
            "write" => {
                let content: String = params
                    .get("content")
                    .ok_or_else(|| crate::tools::lib::ToolError::InvalidParams("content is required for write action".to_string()))?;
                fs::write(&path, &content).map_err(|e| {
                    crate::tools::lib::ToolError::Execution(format!("Failed to write '{}': {}", path, e))
                })?;
                Ok(ToolOutput::Success(serde_json::json!({
                    "path": path,
                    "bytes_written": content.len(),
                    "success": true
                })))
            }
            "list" => {
                let entries: Vec<String> = fs::read_dir(&path)
                    .map_err(|e| {
                        crate::tools::lib::ToolError::Execution(format!(
                            "Failed to list '{}': {}",
                            path, e
                        ))
                    })?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path().to_string_lossy().to_string())
                    .collect();
                Ok(ToolOutput::Success(serde_json::json!({
                    "path": path,
                    "entries": entries
                })))
            }
            _ => Err(crate::tools::lib::ToolError::InvalidParams(
                "action must be 'read', 'write', or 'list'".to_string(),
            )),
        }
    }
}
