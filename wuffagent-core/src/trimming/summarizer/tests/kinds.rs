//! Per-kind summarizer tests (build logs, code, file lists, config gates).

use super::*;

#[test]
fn test_build_log_summarization() {
    let mut lines = Vec::new();
    for i in 0..50 {
        lines.push(format!("Compiling item {} ... ok", i));
    }
    let content = lines.join("\n");
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &make_config());

    assert!(result.contains("lines omitted"));
    assert!(result.len() <= 200);
    assert!(result.starts_with("Compiling item 0"));
    assert!(result.contains("Compiling item 49"));
}

#[test]
fn test_code_summarization() {
    let mut lines = Vec::new();
    for i in 0..100 {
        lines.push(format!("    let x{} = {};", i, i));
    }
    let content = lines.join("\n");
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &make_config());

    assert!(result.contains("lines omitted"));
    assert!(result.len() <= 200);
    assert!(result.starts_with("    let x0"));
}

#[test]
fn test_small_content_not_truncated() {
    let content = "short result";
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(content, &make_config());
    assert_eq!(result, "short result");
}

#[test]
fn test_disabled_config_returns_unmodified() {
    let content = "x".repeat(5000);
    let mut config = make_config();
    config.enabled = false;
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &config);
    assert_eq!(result.len(), 5000);
}

#[test]
fn test_file_list_summarization() {
    let paths: Vec<String> = (0..100)
        .map(|i| format!("/path/to/file_{}.rs", i))
        .collect();
    let content = paths.join("\n");
    let trimming = ContextTrimming::new();
    let result = trimming.summarize_tool_result(&content, &make_config());

    assert!(result.contains("items omitted"));
    assert!(result.len() <= 200);
}
