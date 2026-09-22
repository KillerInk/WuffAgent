//! Fallback tool-call parsing for models that emit text tool calls.
//! Split out of agents/agent.rs (A1).

use super::Agent;

impl Agent {

    /// Parse tool calls from an LLM response.
    /// Uses a bracket-aware parser that tracks both `[`/`]` and `{`/`}`
    /// to correctly handle nested JSON structures.
    pub(crate) fn parse_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        // First try parsing the entire response as JSON directly.
        if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(response) {
            return Some(calls);
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(arr) = obj.get("tool_calls").and_then(|v| v.as_array()) {
                let mut calls: Vec<ToolCall> = Vec::new();
                for v in arr {
                    if let Some(func) = v.get("function") {
                        if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                            if let Some(args_val) = func.get("arguments") {
                                let args_str = match args_val {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                calls.push(ToolCall {
                                    id: v
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    _call_type: "function".to_string(),
                                    function: ToolFunction {
                                        name: name.to_string(),
                                        arguments: args_str,
                                    },
                                });
                            }
                        }
                    }
                }
                if !calls.is_empty() {
                    return Some(calls);
                }
            }
        }

        // Fallback: bracket-aware extraction that tracks both [] and {} depth.
        let mut regions: Vec<(usize, usize)> = Vec::new();
        let mut bracket_depth = 0i32;
        let mut brace_depth = 0i32;
        let mut start: Option<usize> = None;
        let mut start_type: Option<char> = None; // '[' or '{'
        let mut in_string = false;
        let mut escape = false;

        for (i, ch) in response.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' if in_string => {
                    escape = true;
                }
                '"' => {
                    in_string = !in_string;
                }
                c if !in_string && (c == '[' || c == '{') => {
                    if bracket_depth == 0 && brace_depth == 0 {
                        start = Some(i);
                        start_type = Some(c);
                    }
                    if c == '[' {
                        bracket_depth += 1;
                    }
                    if c == '{' {
                        brace_depth += 1;
                    }
                }
                c if !in_string && (c == ']' || c == '}') => {
                    if c == ']' {
                        bracket_depth -= 1;
                    }
                    if c == '}' {
                        brace_depth -= 1;
                    }
                    // Only close a region if we're closing the matching depth-0 opener.
                    if bracket_depth < 0 {
                        bracket_depth = 0;
                    }
                    if brace_depth < 0 {
                        brace_depth = 0;
                    }
                    if bracket_depth == 0 && brace_depth == 0 {
                        if let Some(s) = start {
                            if start_type == Some('[') {
                                regions.push((s, i + 1));
                            }
                            start = None;
                            start_type = None;
                        }
                    }
                }
                _ => {}
            }
        }

        for (s, e) in &regions {
            if e > s {
                let json_str = &response[*s..*e];
                if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                    return Some(calls);
                }
            }
        }

        // Fallback: look for JSON inside markdown code blocks.
        if let Some(start) = response.find("```") {
            let rest = &response[start + 3..];
            if let Some(end) = rest.find("```") {
                let json_str = &rest[..end];
                if let Ok(calls) = serde_json::from_str::<Vec<ToolCall>>(json_str) {
                    return Some(calls);
                }
            }
        }

        None
    }

    /// Extract bash/code blocks from LLM responses and convert them to the
    /// named file tools (read_file, list_dir, search_files, search_content,
    /// mkdir, delete, copy, move, ...). Only commands without a named
    /// equivalent (pwd) fall through to the generic shell tool.
    pub(crate) fn extract_bash_as_tool_calls(&self, response: &str) -> Option<Vec<ToolCall>> {
        let mut calls = Vec::new();
        let mut id_counter = 0u32;

        let mut rest = response;
        while let Some(start) = rest.find("```") {
            rest = &rest[start + 3..];
            if let Some(end) = rest.find("```") {
                let block = &rest[..end];
                rest = &rest[end + 3..];

                if block.trim().starts_with('{') || block.trim().starts_with('[') {
                    continue;
                }

                let lines: Vec<&str> = block.lines().collect();
                let cmd_line = if lines.len() > 1
                    && (lines[0] == "bash" || lines[0] == "sh" || lines[0] == "shell")
                {
                    lines[1..].join("\n").trim().to_string()
                } else {
                    block.trim().to_string()
                };

                if cmd_line.is_empty() {
                    continue;
                }

                let raw_parts: Vec<&str> = cmd_line.split_whitespace().collect();
                if raw_parts.is_empty() {
                    continue;
                }
                let args_parts: Vec<&str> = raw_parts
                    .iter()
                    .skip(1)
                    .filter(|p| !p.starts_with('-'))
                    .copied()
                    .collect();

                // Emit a single tool call with the given name and JSON arguments.
                let mut emit_tool = |name: &str, args: serde_json::Value| {
                    calls.push(ToolCall {
                        id: format!("call_{}", id_counter),
                        _call_type: "function".to_string(),
                        function: ToolFunction {
                            name: name.to_string(),
                            arguments: serde_json::to_string(&args).unwrap_or_default(),
                        },
                    });
                    id_counter += 1;
                };

                let cmd = raw_parts[0];
                match cmd {
                    "ls" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        emit_tool("list_dir", serde_json::json!({ "path": path }));
                    }
                    "cat" => {
                        for path in &args_parts {
                            emit_tool("read_file", serde_json::json!({ "path": path }));
                        }
                    }
                    "find" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        let pattern = format!("{path}/*");
                        emit_tool("search_files", serde_json::json!({ "pattern": pattern }));
                    }
                    "grep" => {
                        // grep [-flags] pattern [path]
                        if let Some(pattern) = args_parts.first() {
                            let path = args_parts.get(1).copied().unwrap_or(".");
                            let is_regex = raw_parts.iter().any(|p| *p == "-E" || *p == "-P");
                            let case_sensitive = !raw_parts.iter().any(|p| *p == "-i");
                            emit_tool(
                                "search_content",
                                serde_json::json!({
                                    "pattern": pattern,
                                    "path": path,
                                    "regex": is_regex,
                                    "case_sensitive": case_sensitive,
                                }),
                            );
                        }
                    }
                    "head" | "tail" => {
                        if let Some(path) = args_parts.last() {
                            emit_tool("read_file", serde_json::json!({ "path": path }));
                        }
                    }
                    "mkdir" => {
                        let path = args_parts.first().copied().unwrap_or(".");
                        let recursive = raw_parts.iter().any(|p| *p == "-p" || *p == "--parents");
                        emit_tool(
                            "mkdir",
                            serde_json::json!({ "path": path, "recursive": recursive }),
                        );
                    }
                    "rm" => {
                        for path in &args_parts {
                            let recursive = raw_parts.iter().any(|p| {
                                *p == "-r"
                                    || *p == "-R"
                                    || *p == "-rf"
                                    || *p == "-fr"
                                    || *p == "--recursive"
                            });
                            emit_tool(
                                "delete",
                                serde_json::json!({ "path": path, "recursive": recursive }),
                            );
                        }
                    }
                    "cp" | "mv" => {
                        if args_parts.len() >= 2 {
                            let name = if cmd == "cp" { "copy" } else { "move" };
                            emit_tool(
                                name,
                                serde_json::json!({
                                    "src": args_parts[0],
                                    "dest": args_parts[args_parts.len() - 1],
                                }),
                            );
                        }
                    }
                    // No named file tool equivalent — run via the shell tool.
                    "pwd" => {
                        emit_tool("shell", serde_json::json!({ "command": cmd_line }));
                    }
                    _ => continue,
                }
            }
        }

        if calls.is_empty() {
            None
        } else {
            Some(calls)
        }
    }
}

/// A tool call from an LLM response.
#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) _call_type: String,
    pub(crate) function: ToolFunction,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct ToolFunction {
    pub(crate) name: String,
    pub(crate) arguments: String,
}
