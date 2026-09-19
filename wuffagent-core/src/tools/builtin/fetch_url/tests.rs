//! Unit tests for the `fetch_url` module (see `super`).

use super::*;

fn params(url: &str, max_bytes: Option<usize>) -> ToolParams {
    let mut p = ToolParams::new();
    p.values
        .insert("url".to_string(), serde_json::json!(url));
    if let Some(mb) = max_bytes {
        p.values
            .insert("max_bytes".to_string(), serde_json::json!(mb));
    }
    p
}

#[test]
fn test_fetch_url_requires_url_param() {
    let tool = FetchUrlTool::new();
    let err = tool.execute(ToolParams::new()).unwrap_err();
    assert!(matches!(err, crate::tools::types::ToolError::InvalidParams(_)));
}

#[test]
fn test_fetch_url_rejects_non_http_scheme() {
    let tool = FetchUrlTool::new();
    let err = tool.execute(params("ftp://example.com/x", None)).unwrap_err();
    assert!(matches!(err, crate::tools::types::ToolError::InvalidParams(_)));
}

#[test]
fn test_fetch_url_schema() {
    let tool = FetchUrlTool::new();
    assert_eq!(tool.name(), "fetch_url");
    let schema = tool.parameters_schema();
    assert_eq!(schema.name, "fetch_url");
    assert!(schema.input_type.as_ref().unwrap().required == vec!["url"]);
}
