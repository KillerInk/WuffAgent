//! One-line display previews for tool calls (live tool cards, transcript
//! headers).
//!
//! These helpers hard-code the builtin tools' argument names, so they belong
//! to the tools brick — the types brick stays free of tool-name knowledge.

/// Format a tool call header for display.
pub fn tool_call_header(name: &str, result: &str) -> String {
    format!(
        "🔧 {}: {}",
        name,
        result.chars().take(80).collect::<String>()
    )
}

/// One-line human-readable preview of a tool call's arguments, for the live
/// tool cards in the UI ("what is the tool doing right now?").
///
/// Picks the most descriptive argument field per tool; falls back to a
/// compact JSON dump. Empty string for empty / unknown argument shapes.
pub fn tool_args_summary(name: &str, arguments: &str) -> String {
    const MAX: usize = 120;
    let trimmed = arguments.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let flat = |s: &str| -> String {
        let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out: String = one_line.chars().take(MAX).collect();
        if one_line.chars().count() > MAX {
            out.push('…');
        }
        out
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // Preferred argument fields per tool (first present field wins).
        let fields: &[&str] = match name {
            "shell" => &["command"],
            "read_file" | "append_file" | "apply_diff" | "write_file" | "delete" | "file_info"
            | "mkdir" | "list_dir" => &["path"],
            "copy" | "move" => &["dest"],
            "search_files" => &["pattern"],
            "search_content" => &["pattern", "path"],
            "web_search" | "search_memory" => &["query"],
            "fetch_url" => &["url"],
            "calculation" => &["expression"],
            "save_memory" | "update_memory" | "consolidate_memories" => &["content"],
            "handoff" => &["task"],
            "restart" => &["reason"],
            _ => &[
                "command",
                "path",
                "query",
                "url",
                "pattern",
                "expression",
                "content",
                "reason",
                "task",
            ],
        };
        for f in fields {
            if let Some(s) = v.get(f).and_then(|x| x.as_str()) {
                if s.trim().is_empty() {
                    continue;
                }
                // search_content: show "pattern in path" when both are given.
                if name == "search_content" && *f == "pattern" {
                    if let Some(p) = v.get("path").and_then(|x| x.as_str()) {
                        if !p.trim().is_empty() {
                            return flat(&format!("{} in {}", s, p));
                        }
                    }
                }
                return flat(s);
            }
        }
        return flat(&v.to_string());
    }
    flat(trimmed)
}

#[cfg(test)]
mod tests;
