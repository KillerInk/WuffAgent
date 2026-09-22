use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

use super::html;

/// Desktop browser user agent for page fetches.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Default maximum body size to read (128 KB).
const DEFAULT_MAX_BYTES: usize = 128 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A tool that fetches a URL and returns its content as plain text
/// (HTML is converted to text). Companion to `web_search`: the agent
/// searches, then reads the promising pages.
pub struct FetchUrlTool {
    http_client: reqwest::Client,
}

/// Cached tokio current-thread runtime for use inside spawn_blocking calls
/// (same pattern as `web_search`: never build a runtime per call).
static BLOCKING_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build blocking runtime")
});

macro_rules! block_on {
    ($expr:expr) => {{
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on($expr),
            Err(_) => BLOCKING_RUNTIME.block_on($expr),
        }
    }};
}

impl FetchUrlTool {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::builder()
                .pool_max_idle_per_host(4)
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client builder"),
        }
    }
}

impl Default for FetchUrlTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for FetchUrlTool {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content as plain text (HTML is converted to text). Params: url (required), max_bytes (optional, default 128KB)"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fetch_url".to_string(),
            description: "Fetch a URL and return its content as plain text".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "url".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "The URL to fetch (http:// or https://)".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "max_bytes".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "integer".to_string(),
                            description: format!(
                                "Maximum number of bytes to read (default {DEFAULT_MAX_BYTES})"
                            ),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["url".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let url: String = params.get("url").ok_or_else(|| {
            crate::tools::types::ToolError::InvalidParams("url is required".to_string())
        })?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(crate::tools::types::ToolError::InvalidParams(format!(
                "url must start with http:// or https://: {url}"
            )));
        }

        let max_bytes: usize = params.get("max_bytes").unwrap_or(DEFAULT_MAX_BYTES);

        let (status, content_type, final_url, body) = block_on!(async {
            let resp = self
                .http_client
                .get(&url)
                .header("User-Agent", BROWSER_UA)
                .header(
                    "Accept",
                    "text/html,text/plain,application/json,text/markdown,*/*;q=0.8",
                )
                .header("Accept-Encoding", "identity")
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|e| {
                    crate::tools::types::ToolError::Execution(format!("HTTP request failed: {e}"))
                })?;
            let status = resp.status().as_u16();
            // Lowercase, params stripped (e.g. `text/html; charset=utf-8` → `text/html`).
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let final_url = resp.url().to_string();
            let body = resp.bytes().await.map_err(|e| {
                crate::tools::types::ToolError::Execution(format!("Failed to read response: {e}"))
            })?;
            Ok::<_, crate::tools::types::ToolError>((status, content_type, final_url, body))
        })?;

        if !(200..300).contains(&status) {
            return Err(crate::tools::types::ToolError::Execution(format!(
                "HTTP {status} for {url}"
            )));
        }

        const ACCEPTED: &[&str] = &[
            "text/html",
            "text/plain",
            "application/json",
            "text/markdown",
        ];
        if !ACCEPTED.iter().any(|p| content_type.starts_with(p)) {
            return Err(crate::tools::types::ToolError::Execution(format!(
                "Unsupported content type: {content_type}"
            )));
        }

        let truncated = body.len() > max_bytes;
        let text_bytes = &body[..max_bytes.min(body.len())];
        let raw = String::from_utf8_lossy(text_bytes);

        let text = if content_type.starts_with("text/html") {
            html::html_to_text(&raw)
        } else {
            raw.trim().to_string()
        };

        Ok(ToolOutput::Success(serde_json::json!({
            "status": status,
            "content_type": content_type,
            "final_url": final_url,
            "truncated": truncated,
            "text": text
        })))
    }
}

#[cfg(test)]
mod tests;
