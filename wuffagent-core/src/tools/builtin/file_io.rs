use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};

use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

/// A tool that performs read/write/list/copy/move/etc. operations on the local filesystem.
pub struct FileIOTool;

/// Validates a path for safety, rejecting dangerous paths and path traversal patterns.
fn validate_path(path: &str) -> Result<(), crate::tools::types::ToolError> {
    // Reject obvious path traversal patterns in the raw path first
    if path.contains("..") {
        return Err(crate::tools::types::ToolError::Execution("Path not allowed".to_string()));
    }

    // Resolve the canonical path to detect path traversal
    let canonical_path = match std::path::Path::new(path).canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // Still check for sensitive directories in the raw path
            let lower = path.to_lowercase().replace('\\', "/");
            if lower == "/etc" || lower.starts_with("/etc/") || lower == "/root" || lower.starts_with("/root/") {
                return Err(crate::tools::types::ToolError::Execution("Path not allowed".to_string()));
            }
            if lower == "c:/windows" || lower.starts_with("c:/windows/") {
                return Err(crate::tools::types::ToolError::Execution("Path not allowed".to_string()));
            }
            return Ok(());
        }
    };

    // Reject absolute paths to sensitive system directories
    let canonical_str = canonical_path.to_string_lossy().to_lowercase();
    // Strip Windows \\?\ prefix from canonicalized paths
    let canonical_normalized = canonical_str.trim_start_matches("\\?\\");
    let canonical_normalized = canonical_normalized.replace('\\', "/");
    if canonical_normalized == "/etc" || canonical_normalized.starts_with("/etc/") || canonical_normalized == "/root" || canonical_normalized.starts_with("/root/") {
        return Err(crate::tools::types::ToolError::Execution("Path not allowed".to_string()));
    }
    // Reject Windows system directories
    if canonical_normalized == "c:/windows" || canonical_normalized.starts_with("c:/windows/") {
        return Err(crate::tools::types::ToolError::Execution("Path not allowed".to_string()));
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

// ─── Action Implementations ───────────────────────────────────────────────────

impl FileIOTool {
    /// Read a text file, optionally limited to a line range (0-indexed, inclusive).
    fn exec_read(&self, path: &str, start_line: Option<usize>, end_line: Option<usize>) -> crate::tools::types::ToolResult<ToolOutput> {
        let content = fs::read_to_string(path).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Failed to read '{}': {}", path, e))
        })?;

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = start_line.unwrap_or(0).min(total);
        let end = end_line.unwrap_or(total).min(total);

        let upper = (end + 1).min(lines.len());
        let slice = if start >= end {
            String::new()
        } else {
            lines[start..upper].join("\n")
        };

        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "content": slice,
            "lines_returned": if start < end { upper - start } else { 0 },
        })))
    }

    /// Write (overwrite) a text file.
    fn exec_write(&self, path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        fs::write(path, content).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Failed to write '{}': {}", path, e))
        })?;
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "bytes_written": content.len(),
            "success": true,
        })))
    }

    /// List entries in a directory.
    fn exec_list(&self, path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        let entries: Vec<String> = fs::read_dir(path)
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to list '{}': {}", path, e))
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "entries": entries,
        })))
    }

    /// Append content to the end of a file.
    fn exec_append(&self, path: &str, content: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to open '{}' for appending: {}", path, e))
            })?
            .write_all(content.as_bytes())
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to append to '{}': {}", path, e))
            })?;
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "bytes_appended": content.len(),
            "success": true,
        })))
    }

    /// Delete a file or an empty directory.
    fn exec_delete(&self, path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        let metadata = fs::metadata(path).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Failed to stat '{}': {}", path, e))
        })?;
        if metadata.is_dir() {
            fs::remove_dir(path).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to remove directory '{}': {} (directory may not be empty)", path, e))
            })?;
        } else {
            fs::remove_file(path).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to remove file '{}': {}", path, e))
            })?;
        }
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "deleted": true,
        })))
    }

    /// Create a directory. If `recursive` is true, create parent directories as needed.
    fn exec_mkdir(&self, path: &str, recursive: bool) -> crate::tools::types::ToolResult<ToolOutput> {
        if recursive {
            fs::create_dir_all(path).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to create directory '{}': {}", path, e))
            })?;
        } else {
            fs::create_dir(path).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to create directory '{}': {}", path, e))
            })?;
        }
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "created": true,
        })))
    }

    /// Copy a file or directory (recursively).
    fn exec_copy(&self, src: &str, dest: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        validate_path(dest)?;
        let metadata = fs::metadata(src).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Source '{}' not found: {}", src, e))
        })?;
        if metadata.is_dir() {
            Self::copy_dir_all(src, dest).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to copy directory '{}': {}", src, e))
            })?;
        } else {
            fs::copy(src, dest).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to copy '{}': {}", src, e))
            })?;
        }
        Ok(ToolOutput::Success(serde_json::json!({
            "src": src,
            "dest": dest,
            "copied": true,
        })))
    }

    /// Recursively copy a directory.
    fn copy_dir_all(src: &str, dest: &str) -> std::io::Result<()> {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dest_path = std::path::Path::new(dest).join(entry.file_name());
            if src_path.is_dir() {
                Self::copy_dir_all(
                    src_path.to_string_lossy().as_ref(),
                    dest_path.to_string_lossy().as_ref(),
                )?;
            } else {
                fs::copy(&src_path, &dest_path)?;
            }
        }
        Ok(())
    }

    /// Move/rename a file or directory.
    fn exec_move(&self, src: &str, dest: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        validate_path(dest)?;
        // Fallback: copy + delete for cross-device moves
        if fs::metadata(src).is_err() {
            return Err(crate::tools::types::ToolError::Execution(format!(
                "Source '{}' not found", src
            )));
        }
        if fs::rename(src, dest).is_err() {
            // Try cross-device fallback
            self.exec_copy(src, dest)?;
            fs::remove_file(src).or_else(|_| fs::remove_dir_all(src)).map_err(|e| {
                crate::tools::types::ToolError::Execution(format!(
                    "Cross-device move failed after copy: {}", e
                ))
            })?;
            return Ok(ToolOutput::Success(serde_json::json!({
                "src": src,
                "dest": dest,
                "moved": true,
                "cross_device": true,
            })));
        }
        Ok(ToolOutput::Success(serde_json::json!({
            "src": src,
            "dest": dest,
            "moved": true,
        })))
    }

    /// Read a file as base64-encoded binary data.
    fn exec_read_binary(&self, path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to open '{}' for binary read: {}", path, e))
            })?
            .read_to_end(&mut bytes)
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to read binary '{}': {}", path, e))
            })?;
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "content_base64": encoded,
            "size": bytes.len(),
        })))
    }

    /// Write base64-decoded binary data to a file.
    fn exec_write_binary(&self, path: &str, content_base64: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, content_base64)
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Invalid base64 content: {}", e))
            })?;
        fs::write(path, &bytes).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Failed to write binary '{}': {}", path, e))
        })?;
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "bytes_written": bytes.len(),
            "success": true,
        })))
    }

    /// Glob search with `*`, `?`, `[cls]`, and `**` (recursive) support.
    fn exec_glob(&self, pattern: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        let matches: Vec<String> = glob::glob(pattern)
            .map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Invalid glob pattern '{}': {}", pattern, e))
            })?
            .filter_map(|m| m.ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        Ok(ToolOutput::Success(serde_json::json!({
            "pattern": pattern,
            "matches": matches,
            "count": matches.len(),
        })))
    }

    /// Apply a unified diff/patch string to an existing file.
    ///
    /// Supports standard `diff -u` output with `@@` hunk headers.
    /// Each hunk contains context lines (prefixed with ` `), additions (`+`), and deletions (`-`).
    fn exec_diff_apply(&self, path: &str, diff: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        // Parse the unified diff and apply it line by line.
        let lines: Vec<&str> = diff.lines().collect();
        let mut result_lines: Vec<String> = Vec::new();
        let mut file_line_idx: usize = 0;
        let file_content = fs::read_to_string(path).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!(
                "Failed to read '{}' for diff apply: {}", path, e
            ))
        })?;
        let ends_with_newline = file_content.ends_with('\n');
        let file_lines: Vec<&str> = file_content.lines().collect();
        let mut lines_changed: u32 = 0;
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            // Skip file header lines (--- / +++ / diff --git)
            if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("diff ") {
                i += 1;
                continue;
            }

            // Skip empty lines (e.g. leading/trailing newlines in diff string)
            if line.is_empty() {
                i += 1;
                continue;
            }

            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            if line.starts_with("@@") {
                // Extract new file start and count
                let new_start = Self::parse_hunk_new_start(line);
                let new_count = Self::parse_hunk_new_count(line);
                if let (Some(ns), Some(_nc)) = (new_start, new_count) {
                    // Advance file_line_idx to the hunk's starting position
                    let target_idx = ns.saturating_sub(1); // 1-indexed to 0-indexed
                    if target_idx > file_line_idx {
                        // Copy lines that are not part of this hunk
                        for line in &file_lines[file_line_idx..target_idx] {
                            result_lines.push(line.to_string());
                        }
                        file_line_idx = target_idx;
                    }
                    // Process hunk body
                    i += 1;
                    while i < lines.len() {
                        let hunk_line = lines[i];
                        if hunk_line.starts_with("@@") || hunk_line.starts_with("diff ") ||
                           hunk_line.starts_with("--- ") || hunk_line.starts_with("+++ ") {
                            break;
                        }
                        match hunk_line.chars().next() {
                            Some('+') => {
                                // Addition: insert the line
                                result_lines.push(hunk_line[1..].to_string());
                                lines_changed += 1;
                            }
                            Some('-') => {
                                // Deletion: skip the line (consume from file)
                                if file_line_idx < file_lines.len() {
                                    file_line_idx += 1;
                                    lines_changed += 1;
                                }
                            }
                            Some(' ') | None => {
                                // Context line: strip leading space and use diff content
                                let content = if hunk_line.is_empty() {
                                    String::new()
                                } else {
                                    hunk_line[1..].to_string()
                                };
                                result_lines.push(content);
                                if file_line_idx < file_lines.len() {
                                    file_line_idx += 1;
                                }
                            }
                            _ => {
                                // Unknown prefix, treat as context
                                if file_line_idx < file_lines.len() {
                                    result_lines.push(file_lines[file_line_idx].to_string());
                                    file_line_idx += 1;
                                }
                            }
                        }
                        i += 1;
                    }
                    continue;
                }
            }

            // Lines before any hunk (file header / unchanged preamble)
            result_lines.push(line.to_string());
            i += 1;
        }

        // Append any remaining file lines after the last hunk
        while file_line_idx < file_lines.len() {
            result_lines.push(file_lines[file_line_idx].to_string());
            file_line_idx += 1;
        }

        let mut final_content = result_lines.join("\n");
        if ends_with_newline {
            final_content.push('\n');
        }
        fs::write(path, &final_content).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Failed to write patched file '{}': {}", path, e))
        })?;

        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "lines_changed": lines_changed,
            "success": true,
        })))
    }

    /// Parse the new-file start line from a hunk header like `@@ -1,5 +3,4 @@`.
    fn parse_hunk_new_start(line: &str) -> Option<usize> {
        let at_at = line.find("@@")?;
        let rest = &line[at_at + 2..].trim();
        // rest starts with something like "-1,5 +3,4"
        let plus_pos = rest.find('+')?;
        let num_part = &rest[plus_pos + 1..];
        let comma = num_part.find(',');
        let num_str = match comma {
            Some(idx) => &num_part[..idx],
            None => num_part,
        };
        num_str.trim().parse::<usize>().ok()
    }

    /// Parse the new-file line count from a hunk header.
    fn parse_hunk_new_count(line: &str) -> Option<usize> {
        let at_at = line.find("@@")?;
        let rest = &line[at_at + 2..].trim();
        let plus_pos = rest.find('+')?;
        let num_part = &rest[plus_pos + 1..];
        if let Some(comma) = num_part.find(',') {
            let count_str = &num_part[comma + 1..];
            // Trim trailing characters (e.g. " @" in "@@ -1,3 +1,3 @@")
            let count_str = count_str.trim_end_matches(|c: char| c.is_whitespace() || c == '@');
            count_str.trim().parse::<usize>().ok()
        } else {
            // Single number means count=1
            Some(1)
        }
    }

    /// Get file metadata as JSON.
    fn exec_file_info(&self, path: &str) -> crate::tools::types::ToolResult<ToolOutput> {
        let metadata = fs::metadata(path).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Failed to stat '{}': {}", path, e))
        })?;
        let perms = metadata.permissions();
        #[cfg(unix)]
        let perm_str = {
            use std::os::unix::fs::PermissionsExt;
            format!("{:04o}", perms.mode())
        };
        #[cfg(windows)]
        let perm_str = {
            let mut s = String::new();
            if perms.readonly() { s.push_str("readonly"); } else { s.push_str("rw"); }
            s
        };
        let mtime = metadata.modified()
            .ok()
            .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
            .unwrap_or(0);
        Ok(ToolOutput::Success(serde_json::json!({
            "path": path,
            "size": metadata.len(),
            "mtime": mtime,
            "is_dir": metadata.is_dir(),
            "is_file": metadata.is_file(),
            "permissions": perm_str,
        })))
    }
}

impl Tool for FileIOTool {
    fn name(&self) -> &str {
        "file_io"
    }

    fn description(&self) -> &str {
        "Read, write, list, copy, move, delete, mkdir, append, glob, diff-apply, and stat files on the local system"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_io".to_string(),
            description: "Comprehensive file I/O: read (with optional line range), write, list, append, delete, mkdir, copy, move, rename, read_binary, write_binary, glob, diff_apply, file_info".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    // Common params
                    map.insert(
                        "action".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Action: read, write, list, append, delete, mkdir, copy, move, rename, read_binary, write_binary, glob, diff_apply, file_info".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "path".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Target file or directory path (used by: read, write, list, append, delete, mkdir, read_binary, write_binary, diff_apply, file_info)".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "content".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Content to write/append (required for write and append actions)".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "src".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Source path (required for copy, move, rename actions)".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "dest".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Destination path (required for copy, move, rename actions)".to_string(),
                            nullable: true,
                        },
                    );
                    // Action-specific params
                    map.insert(
                        "start_line".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "integer".to_string(),
                            description: "Start line (0-indexed, inclusive) for read action. Defaults to 0.".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "end_line".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "integer".to_string(),
                            description: "End line (0-indexed, inclusive) for read action. Defaults to EOF.".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "recursive".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "boolean".to_string(),
                            description: "Create parent directories for mkdir (default false).".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "content_base64".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Base64-encoded binary content (required for write_binary action)".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "pattern".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Glob pattern (e.g. '**/*.rs', 'src/**/*.txt') for glob action".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "diff".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Unified diff/patch string to apply (required for diff_apply action)".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["action".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let action: String = params
            .get("action")
            .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("action is required".to_string()))?;

        match action.as_str() {
            "read" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for read action".to_string()))?;
                validate_path(&path)?;
                let start_line: Option<usize> = params.get("start_line");
                let end_line: Option<usize> = params.get("end_line");
                self.exec_read(&path, start_line, end_line)
            }
            "write" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for write action".to_string()))?;
                validate_path(&path)?;
                let content: String = params
                    .get("content")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("content is required for write action".to_string()))?;
                self.exec_write(&path, &content)
            }
            "list" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for list action".to_string()))?;
                validate_path(&path)?;
                self.exec_list(&path)
            }
            "append" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for append action".to_string()))?;
                validate_path(&path)?;
                let content: String = params
                    .get("content")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("content is required for append action".to_string()))?;
                self.exec_append(&path, &content)
            }
            "delete" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for delete action".to_string()))?;
                validate_path(&path)?;
                self.exec_delete(&path)
            }
            "mkdir" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for mkdir action".to_string()))?;
                validate_path(&path)?;
                let recursive: bool = params.get("recursive").unwrap_or(false);
                self.exec_mkdir(&path, recursive)
            }
            "copy" => {
                let src: String = params
                    .get("src")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("src is required for copy action".to_string()))?;
                validate_path(&src)?;
                let dest: String = params
                    .get("dest")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("dest is required for copy action".to_string()))?;
                self.exec_copy(&src, &dest)
            }
            "move" => {
                let src: String = params
                    .get("src")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("src is required for move action".to_string()))?;
                validate_path(&src)?;
                let dest: String = params
                    .get("dest")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("dest is required for move action".to_string()))?;
                self.exec_move(&src, &dest)
            }
            "rename" => {
                let src: String = params
                    .get("src")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("src is required for rename action".to_string()))?;
                validate_path(&src)?;
                let dest: String = params
                    .get("dest")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("dest is required for rename action".to_string()))?;
                self.exec_move(&src, &dest)
            }
            "read_binary" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for read_binary action".to_string()))?;
                validate_path(&path)?;
                self.exec_read_binary(&path)
            }
            "write_binary" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for write_binary action".to_string()))?;
                validate_path(&path)?;
                let content_base64: String = params
                    .get("content_base64")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("content_base64 is required for write_binary action".to_string()))?;
                self.exec_write_binary(&path, &content_base64)
            }
            "glob" => {
                let pattern: String = params
                    .get("pattern")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("pattern is required for glob action".to_string()))?;
                self.exec_glob(&pattern)
            }
            "diff_apply" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for diff_apply action".to_string()))?;
                validate_path(&path)?;
                let diff: String = params
                    .get("diff")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("diff is required for diff_apply action".to_string()))?;
                self.exec_diff_apply(&path, &diff)
            }
            "file_info" => {
                let path: String = params
                    .get("path")
                    .ok_or_else(|| crate::tools::types::ToolError::InvalidParams("path is required for file_info action".to_string()))?;
                validate_path(&path)?;
                self.exec_file_info(&path)
            }
            _ => Err(crate::tools::types::ToolError::InvalidParams(
                "action must be one of: read, write, list, append, delete, mkdir, copy, move, rename, read_binary, write_binary, glob, diff_apply, file_info".to_string(),
            )),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Path Validation Tests ──────────────────────────────────────────────

    #[test]
    fn test_validate_path_rejects_etc() {
        assert!(validate_path("/etc/passwd").is_err());
        assert!(validate_path("/etc/shadow").is_err());
    }

    #[test]
    fn test_validate_path_rejects_windows_system() {
        assert!(validate_path("C:\\Windows\\System32").is_err());
        assert!(validate_path("c:\\windows").is_err());
    }

    #[test]
    fn test_validate_path_rejects_traversal() {
        assert!(validate_path("../../etc/passwd").is_err());
        assert!(validate_path("foo/..").is_err());
    }

    #[test]
    fn test_validate_path_accepts_safe_paths() {
        assert!(validate_path("/tmp").is_ok());
        assert!(validate_path("./relative").is_ok());
        assert!(validate_path("C:\\Users\\test").is_ok());
    }

    // ── Read Tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_file_io_read_missing_file() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!("nonexistent_file_12345.txt"));
                m.insert("action".to_string(), json!("read"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    #[test]
    fn test_file_io_write_and_read() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let content = "hello wuffagent test";

        let tool = FileIOTool::new();

        // Write
        let write_params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("write"));
                m.insert("content".to_string(), json!(content));
                m
            },
        };
        match tool.execute(write_params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["bytes_written"], content.len());
                assert_eq!(v["success"], true);
            }
            _ => panic!("write should succeed"),
        }

        // Read back
        let read_params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("read"));
                m
            },
        };
        match tool.execute(read_params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["content"], content);
            }
            _ => panic!("read should succeed"),
        }
    }

    #[test]
    fn test_file_io_read_with_line_range() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let content = "line0\nline1\nline2\nline3\nline4\n";
        fs::write(&path, content).unwrap();

        let tool = FileIOTool::new();

        // Read lines 1..3 (0-indexed, inclusive end)
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("read"));
                m.insert("start_line".to_string(), json!(1));
                m.insert("end_line".to_string(), json!(3));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["content"], "line1\nline2\nline3");
                assert_eq!(v["lines_returned"], 3);
            }
            _ => panic!("read with line range should succeed"),
        }

        // Read from line 2 to EOF
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("read"));
                m.insert("start_line".to_string(), json!(2));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["content"], "line2\nline3\nline4");
            }
            _ => panic!("read from line 2 should succeed"),
        }

        // Read with end_line beyond file length → clamps
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("read"));
                m.insert("start_line".to_string(), json!(0));
                m.insert("end_line".to_string(), json!(999));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["content"], content.trim_end_matches('\n'));
            }
            _ => panic!("read with out-of-range end_line should succeed"),
        }
    }

    #[test]
    fn test_file_io_read_empty_range() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        fs::write(&path, "only line\n").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("read"));
                m.insert("start_line".to_string(), json!(5));
                m.insert("end_line".to_string(), json!(3));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["content"], "");
                assert_eq!(v["lines_returned"], 0);
            }
            _ => panic!("empty range should succeed"),
        }
    }

    // ── List Tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_file_io_list_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();
        fs::write(dir_path.join("a.txt"), "a").unwrap();
        fs::write(dir_path.join("b.txt"), "b").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(dir_path.to_string_lossy().to_string()));
                m.insert("action".to_string(), json!("list"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                let entries: Vec<String> = serde_json::from_value(v["entries"].clone()).unwrap();
                assert!(entries.iter().any(|e| e.contains("a.txt")));
                assert!(entries.iter().any(|e| e.contains("b.txt")));
            }
            _ => panic!("list should succeed"),
        }
    }

    // ── Append Tests ───────────────────────────────────────────────────────

    #[test]
    fn test_file_io_append() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        fs::write(&path, "first line\n").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("append"));
                m.insert("content".to_string(), json!("second line\n"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["bytes_appended"], 12);
                assert_eq!(v["success"], true);
            }
            _ => panic!("append should succeed"),
        }

        // Verify content
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "first line\nsecond line\n");
    }

    #[test]
    fn test_file_io_append_creates_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("new_file.txt");
        let path_str = path.to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path_str));
                m.insert("action".to_string(), json!("append"));
                m.insert("content".to_string(), json!("hello"));
                m
            },
        };
        assert!(tool.execute(params).is_ok());
        assert!(path.exists());
    }

    // ── Delete Tests ───────────────────────────────────────────────────────

    #[test]
    fn test_file_io_delete_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("delete"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["deleted"], true);
            }
            _ => panic!("delete should succeed"),
        }
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn test_file_io_delete_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("delete"));
                m
            },
        };
        assert!(tool.execute(params).is_ok());
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn test_file_io_delete_non_empty_dir_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "data").unwrap();
        let path = dir.path().to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("delete"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    // ── Mkdir Tests ────────────────────────────────────────────────────────

    #[test]
    fn test_file_io_mkdir() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("a").join("b");
        let path = sub.to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("mkdir"));
                m.insert("recursive".to_string(), json!(true));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["created"], true);
            }
            _ => panic!("mkdir should succeed"),
        }
        assert!(sub.exists());
    }

    #[test]
    fn test_file_io_mkdir_non_recursive_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("a").join("b");
        let path = sub.to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("mkdir"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    // ── Copy Tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_file_io_copy_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.txt");
        let dest = tmp.path().join("dest.txt");
        fs::write(&src, "copy me").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("src".to_string(), json!(src.to_string_lossy().to_string()));
                m.insert("dest".to_string(), json!(dest.to_string_lossy().to_string()));
                m.insert("action".to_string(), json!("copy"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["copied"], true);
            }
            _ => panic!("copy should succeed"),
        }
        assert_eq!(fs::read_to_string(&dest).unwrap(), "copy me");
    }

    #[test]
    fn test_file_io_copy_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("srcdir");
        let dest = tmp.path().join("destdir");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        let sub = src.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("b.txt"), "b").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("src".to_string(), json!(src.to_string_lossy().to_string()));
                m.insert("dest".to_string(), json!(dest.to_string_lossy().to_string()));
                m.insert("action".to_string(), json!("copy"));
                m
            },
        };
        assert!(tool.execute(params).is_ok());
        assert!(dest.join("a.txt").exists());
        assert!(dest.join("sub/b.txt").exists());
    }

    // ── Move / Rename Tests ────────────────────────────────────────────────

    #[test]
    fn test_file_io_move_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        let dest = tmp.path().join("b.txt");
        fs::write(&src, "move me").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("src".to_string(), json!(src.to_string_lossy().to_string()));
                m.insert("dest".to_string(), json!(dest.to_string_lossy().to_string()));
                m.insert("action".to_string(), json!("move"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["moved"], true);
            }
            _ => panic!("move should succeed"),
        }
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "move me");
    }

    #[test]
    fn test_file_io_rename_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("old.txt");
        let dest = tmp.path().join("new.txt");
        fs::write(&src, "rename me").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("src".to_string(), json!(src.to_string_lossy().to_string()));
                m.insert("dest".to_string(), json!(dest.to_string_lossy().to_string()));
                m.insert("action".to_string(), json!("rename"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["moved"], true);
            }
            _ => panic!("rename should succeed"),
        }
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "rename me");
    }

    // ── Binary Tests ───────────────────────────────────────────────────────

    #[test]
    fn test_file_io_read_write_binary() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let raw_bytes = vec![0u8, 1, 2, 255, 128, 0];
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw_bytes);

        // Write binary
        let tool = FileIOTool::new();
        let write_params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("write_binary"));
                m.insert("content_base64".to_string(), json!(&encoded));
                m
            },
        };
        match tool.execute(write_params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["bytes_written"], 6);
            }
            _ => panic!("write_binary should succeed"),
        };

        // Read binary
        let read_params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("read_binary"));
                m
            },
        };
        match tool.execute(read_params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["content_base64"], encoded);
                assert_eq!(v["size"], 6);
            }
            _ => panic!("read_binary should succeed"),
        }
    }

    #[test]
    fn test_file_io_write_binary_invalid_base64() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bin.dat");
        let path_str = path.to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path_str));
                m.insert("action".to_string(), json!("write_binary"));
                m.insert("content_base64".to_string(), json!("!!!not-base64!!!"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    // ── Glob Tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_file_io_glob_simple() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "").unwrap();
        fs::write(tmp.path().join("b.txt"), "").unwrap();
        fs::write(tmp.path().join("c.rs"), "").unwrap();

        let tool = FileIOTool::new();
        let pattern = tmp.path().join("*.txt").to_string_lossy().to_string();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("pattern".to_string(), json!(&pattern));
                m.insert("action".to_string(), json!("glob"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                let matches: Vec<String> = serde_json::from_value(v["matches"].clone()).unwrap();
                assert_eq!(matches.len(), 2);
                assert_eq!(v["count"], 2);
            }
            _ => panic!("glob should succeed"),
        }
    }

    #[test]
    fn test_file_io_glob_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), "").unwrap();
        fs::write(tmp.path().join("sub/b.txt"), "").unwrap();
        fs::write(tmp.path().join("sub/c.rs"), "").unwrap();

        let tool = FileIOTool::new();
        let pattern = tmp.path().join("**/*.txt").to_string_lossy().to_string();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("pattern".to_string(), json!(&pattern));
                m.insert("action".to_string(), json!("glob"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                let matches: Vec<String> = serde_json::from_value(v["matches"].clone()).unwrap();
                assert!(matches.len() >= 2);
            }
            _ => panic!("glob recursive should succeed"),
        }
    }

    // ── Diff Apply Tests ───────────────────────────────────────────────────

    #[test]
    fn test_file_io_diff_apply_basic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        fs::write(&path, "line1\nline2\nline3\n").unwrap();

        let diff = "
--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,3 @@
 line1
+added line
 line2
 line3
";
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("diff_apply"));
                m.insert("diff".to_string(), json!(diff));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["lines_changed"], 1);
                assert_eq!(v["success"], true);
            }
            _ => panic!("diff_apply should succeed"),
        }
        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "line1\nadded line\nline2\nline3\n");
    }

    #[test]
    fn test_file_io_diff_apply_delete_lines() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        fs::write(&path, "line1\nline2\nline3\n").unwrap();

        let diff = "
--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,2 @@
 line1
-line2
 line3
";
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("diff_apply"));
                m.insert("diff".to_string(), json!(diff));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["lines_changed"], 1);
            }
            _ => panic!("diff_apply delete should succeed"),
        }
        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "line1\nline3\n");
    }

    #[test]
    fn test_file_io_diff_apply_missing_file_fails() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!("nonexistent_diff_target.txt"));
                m.insert("action".to_string(), json!("diff_apply"));
                m.insert("diff".to_string(), json!("@@ -1,1 +1,1 @@\n-old\n+new\n"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    // ── File Info Tests ────────────────────────────────────────────────────

    #[test]
    fn test_file_io_file_info() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        fs::write(&path, "hello").unwrap();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("file_info"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["size"], 5);
                assert_eq!(v["is_file"], true);
                assert_eq!(v["is_dir"], false);
                assert!(v["mtime"].as_u64().unwrap() > 0);
            }
            _ => panic!("file_info should succeed"),
        }
    }

    #[test]
    fn test_file_io_file_info_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();

        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!(&path));
                m.insert("action".to_string(), json!("file_info"));
                m
            },
        };
        match tool.execute(params) {
            Ok(ToolOutput::Success(v)) => {
                assert_eq!(v["is_dir"], true);
                assert_eq!(v["is_file"], false);
            }
            _ => panic!("file_info on dir should succeed"),
        }
    }

    // ── Error / Validation Tests ───────────────────────────────────────────

    #[test]
    fn test_file_io_invalid_action() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!("/tmp"));
                m.insert("action".to_string(), json!("nonexistent"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    #[test]
    fn test_file_io_missing_action_param() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!("/tmp"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    #[test]
    fn test_file_io_missing_path_for_read() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("action".to_string(), json!("read"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    #[test]
    fn test_file_io_missing_content_for_write() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!("/tmp/test.txt"));
                m.insert("action".to_string(), json!("write"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    #[test]
    fn test_file_io_validate_path_blocks_etc_on_delete() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), json!("/etc/passwd"));
                m.insert("action".to_string(), json!("delete"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }

    #[test]
    fn test_file_io_validate_path_blocks_traversal_on_copy() {
        let tool = FileIOTool::new();
        let params = ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("src".to_string(), json!("./safe.txt"));
                m.insert("dest".to_string(), json!("../../etc/passwd"));
                m.insert("action".to_string(), json!("copy"));
                m
            },
        };
        assert!(tool.execute(params).is_err());
    }
}