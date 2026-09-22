//! Builtin file tools: the thin XxxTool structs (name/schema/execute)
//! forwarding to the ops functions, plus the shared schema-building
//! helpers. Pure code motion from the old file_io.rs (F1).

mod common;
mod ops;

pub(crate) use common::*;
pub(crate) use ops::*;

use std::collections::HashMap;

use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};

/// Build a ToolSchema from a flat list of (name, type, description, nullable) fields.
pub(crate) fn build_schema(
    name: &str,
    desc: &str,
    props: &[(&str, &str, &str, bool)],
    required: &[&str],
) -> ToolSchema {
    let mut map = HashMap::new();
    for (k, t, d, n) in props {
        map.insert(
            k.to_string(),
            FieldSchema {
                type_name: t.to_string(),
                description: d.to_string(),
                nullable: *n,
            },
        );
    }
    ToolSchema {
        name: name.to_string(),
        description: desc.to_string(),
        input_type: Some(JsonSchema {
            type_name: "object".to_string(),
            properties: Some(map),
            required: required.iter().map(|s| s.to_string()).collect(),
        }),
    }
}

pub(crate) fn required_str(params: &ToolParams, key: &str) -> Result<String, ToolError> {
    params
        .get(key)
        .ok_or_else(|| ToolError::InvalidParams(format!("{key} is required")))
}

/// Read a text file, optionally limited to a line range.
pub struct ReadFileTool;

impl ReadFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a text file, optionally limited to a line range. Returns the file content; large files are truncated with a `truncated` flag and `total_lines` for paging. A UTF-8 BOM is stripped from the first line (reported in the `bom` flag); CRLF files are shown as LF lines."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "read_file",
            "Read a text file, optionally limited to a line range",
            &[
                ("path", "string", "Path of the file to read.", false),
                ("start_line", "integer", "First line to read (1-indexed). Defaults to 1.", true),
                ("end_line", "integer", "Last line to read (1-indexed, inclusive). Defaults to end of file. Use with total_lines to page through large files.", true),
                ("line_numbers", "boolean", "Prefix each line with its 1-based line number. Defaults to true.", true),
            ],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let start_line: Option<usize> = params.get("start_line");
        let end_line: Option<usize> = params.get("end_line");
        let line_numbers: bool = params.get("line_numbers").unwrap_or(true);
        read_file(&path, start_line, end_line, line_numbers)
    }
}

/// Write (overwrite) a text file.
pub struct WriteFileTool;

impl WriteFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write (overwrite) a text file. The full content must be provided. Parent directories are created if missing; the write is atomic (temp file + rename), so a failure never leaves a truncated file. When overwriting an existing file, its BOM and dominant line ending (CRLF/LF) are preserved, so LF content rewrites a CRLF file as CRLF."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "write_file",
            "Write (overwrite) a text file",
            &[
                ("path", "string", "Path of the file to write.", false),
                (
                    "content",
                    "string",
                    "Full content to write to the file.",
                    false,
                ),
            ],
            &["path", "content"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let content = required_str(&params, "content")?;
        write_file(&path, &content)
    }
}

/// Append content to the end of a file.
pub struct AppendFileTool;

impl AppendFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for AppendFileTool {
    fn name(&self) -> &str {
        "append_file"
    }
    fn description(&self) -> &str {
        "Append content to the end of a file (creates the file if it does not exist). Line endings are matched to the existing file, and a missing trailing line break in the existing file is added before the appended content."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "append_file",
            "Append content to the end of a file",
            &[
                ("path", "string", "Path of the file to append to.", false),
                ("content", "string", "Content to append.", false),
            ],
            &["path", "content"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let content = required_str(&params, "content")?;
        append_file(&path, &content)
    }
}

/// List entries in a directory.
pub struct ListDirTool;

impl ListDirTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List the entries (files and directories) inside a directory with type and file size; directories are listed first."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "list_dir",
            "List the entries inside a directory",
            &[("path", "string", "Path of the directory to list.", false)],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        list_dir(&path)
    }
}

/// Glob search for files.
pub struct SearchFilesTool;

impl SearchFilesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }
    fn description(&self) -> &str {
        "Find files matching a glob pattern (supports *, ?, [cls], and ** recursive). Skips .git and target directories; results are capped (default 500)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "search_files",
            "Find files matching a glob pattern",
            &[
                (
                    "pattern",
                    "string",
                    "Glob pattern to match files against.",
                    false,
                ),
                (
                    "max_results",
                    "integer",
                    "Maximum number to return. Defaults to 500.",
                    true,
                ),
            ],
            &["pattern"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let pattern = required_str(&params, "pattern")?;
        let max_results: Option<usize> = params.get("max_results");
        search_files(&pattern, max_results)
    }
}

/// Apply targeted edits to a file via SEARCH/REPLACE blocks.
pub struct ApplyDiffTool;

impl ApplyDiffTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for ApplyDiffTool {
    fn name(&self) -> &str {
        "apply_diff"
    }
    fn description(&self) -> &str {
        "Apply targeted edits to an existing file using SEARCH/REPLACE blocks. \
         Each block's SEARCH text must match exactly one place in the file. \
         Use read_file first to copy the exact current text. \
         The file's line ending (CRLF/LF) and UTF-8 BOM are preserved."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "apply_diff",
            "Apply targeted edits to an existing file using SEARCH/REPLACE blocks",
            &[
                ("path", "string", "Path of the file to edit.", false),
                ("diff", "string",
                 "One or more SEARCH/REPLACE blocks in the form:\n\
                  <<<<<<< SEARCH\n<exact text to find (must be unique)>\n=======\n<replacement text>\n>>>>>>> REPLACE\n\
                  The SEARCH text must match exactly once in the file; provide enough surrounding context to be unique.",
                 false),
            ],
            &["path", "diff"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let diff = required_str(&params, "diff")?;
        apply_diff(&path, &diff)
    }
}

/// Create a directory.
pub struct MkdirTool;

impl MkdirTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for MkdirTool {
    fn name(&self) -> &str {
        "mkdir"
    }
    fn description(&self) -> &str {
        "Create a directory (parent directories are created as needed when recursive is true)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "mkdir",
            "Create a directory",
            &[
                ("path", "string", "Path of the directory to create.", false),
                (
                    "recursive",
                    "boolean",
                    "Create parent directories as needed. Defaults to true.",
                    true,
                ),
            ],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let recursive: bool = params.get("recursive").unwrap_or(true);
        mkdir(&path, recursive)
    }
}

/// Delete a file or directory.
pub struct DeleteTool;

impl DeleteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for DeleteTool {
    fn name(&self) -> &str {
        "delete"
    }
    fn description(&self) -> &str {
        "Delete a file or directory. Deleting a non-empty directory requires recursive: true."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "delete",
            "Delete a file or directory",
            &[
                (
                    "path",
                    "string",
                    "Path of the file or directory to delete.",
                    false,
                ),
                (
                    "recursive",
                    "boolean",
                    "Delete directories recursively with all their contents. Defaults to false.",
                    true,
                ),
            ],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        let recursive: bool = params.get("recursive").unwrap_or(false);
        delete(&path, recursive)
    }
}

/// Copy a file or directory.
pub struct CopyTool;

impl CopyTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for CopyTool {
    fn name(&self) -> &str {
        "copy"
    }
    fn description(&self) -> &str {
        "Copy a file or directory (directories are copied recursively)."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "copy",
            "Copy a file or directory",
            &[
                (
                    "src",
                    "string",
                    "Path of the source file or directory.",
                    false,
                ),
                (
                    "dest",
                    "string",
                    "Path of the destination file or directory.",
                    false,
                ),
            ],
            &["src", "dest"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let src = required_str(&params, "src")?;
        if let Err(e) = validate_path(&src) {
            return Err(e);
        }
        let dest = required_str(&params, "dest")?;
        if let Err(e) = validate_path(&dest) {
            return Err(e);
        }
        copy(&src, &dest)
    }
}

/// Move or rename a file or directory.
pub struct MoveTool;

impl MoveTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for MoveTool {
    fn name(&self) -> &str {
        "move"
    }
    fn description(&self) -> &str {
        "Move or rename a file or directory."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "move",
            "Move or rename a file or directory",
            &[
                (
                    "src",
                    "string",
                    "Current path of the file or directory.",
                    false,
                ),
                (
                    "dest",
                    "string",
                    "New path for the file or directory.",
                    false,
                ),
            ],
            &["src", "dest"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let src = required_str(&params, "src")?;
        if let Err(e) = validate_path(&src) {
            return Err(e);
        }
        let dest = required_str(&params, "dest")?;
        if let Err(e) = validate_path(&dest) {
            return Err(e);
        }
        move_item(&src, &dest)
    }
}

/// Get file or directory metadata.
pub struct FileInfoTool;

impl FileInfoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for FileInfoTool {
    fn name(&self) -> &str {
        "file_info"
    }
    fn description(&self) -> &str {
        "Get file or directory metadata: size, last-modified time, type, and permissions."
    }
    fn parameters_schema(&self) -> ToolSchema {
        build_schema(
            "file_info",
            "Get file or directory metadata",
            &[(
                "path",
                "string",
                "Path of the file or directory to inspect.",
                false,
            )],
            &["path"],
        )
    }
    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let path = required_str(&params, "path")?;
        if let Err(e) = validate_path(&path) {
            return Err(e);
        }
        file_info(&path)
    }
}

#[cfg(test)]
mod tests;
