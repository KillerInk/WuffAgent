//! Unit tests for `tools::preview` (see `super`).

use super::*;

/// Args preview picks the descriptive field per tool.
#[test]
fn tool_args_summary_picks_descriptive_fields() {
    assert_eq!(
        tool_args_summary("shell", r#"{"command":"git log -1 --stat"}"#),
        "git log -1 --stat"
    );
    assert_eq!(
        tool_args_summary("read_file", r#"{"path":"/tmp/a.rs"}"#),
        "/tmp/a.rs"
    );
    assert_eq!(
        tool_args_summary("search_content", r#"{"pattern":"foo","path":"src"}"#),
        "foo in src"
    );
    assert_eq!(
        tool_args_summary("web_search", r#"{"query":"rust async"}"#),
        "rust async"
    );
    // Unknown tool: first preferred field present wins.
    assert_eq!(
        tool_args_summary("mcp_thing", r#"{"url":"https://x.dev"}"#),
        "https://x.dev"
    );
    // Unparseable JSON: flattened raw text.
    assert_eq!(tool_args_summary("x", "hello world"), "hello world");
    // Empty shapes: empty preview.
    assert_eq!(tool_args_summary("shell", ""), "");
    assert_eq!(tool_args_summary("shell", "{}"), "");
    // Long values are truncated with an ellipsis.
    let long = "a".repeat(300);
    let out = tool_args_summary("shell", &format!(r#"{{"command":"{}"}}"#, long));
    assert!(out.chars().count() <= 121);
    assert!(out.ends_with('…'));
}
