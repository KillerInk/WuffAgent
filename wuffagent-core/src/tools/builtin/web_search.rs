use std::collections::HashMap;

use crate::tools::lib::{Tool, ToolOutput, ToolParams, ToolSchema};

/// Search backend configuration.
#[derive(Clone, Debug)]
pub enum SearchBackend {
    /// DuckDuckGo HTML search (with session warming to bypass bot detection).
    DuckDuckGo,
    /// Self-hosted SearXNG instance with JSON API.
    SearXNG { base_url: String },
    /// Brave Search API (requires API key).
    Brave { api_key: String },
}

/// A tool that searches the web via a configurable search endpoint.
pub struct WebSearchTool {
    http_client: reqwest::Client,
    backend: SearchBackend,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::new(),
            backend: SearchBackend::DuckDuckGo,
        }
    }

    pub fn with_backend(mut self, backend: SearchBackend) -> Self {
        self.backend = backend;
        self
    }

    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = client;
        self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information using a search engine"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".to_string(),
            description: "Search the web for information".to_string(),
            input_type: Some(crate::tools::lib::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "query".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Search query".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "max_results".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "integer".to_string(),
                            description: "Maximum number of results to return".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["query".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::lib::ToolResult<ToolOutput> {
        let query: String = params
            .get("query")
            .ok_or_else(|| {
                crate::tools::lib::ToolError::InvalidParams("query is required".to_string())
            })?;

        let max_results: u32 = params.get("max_results").unwrap_or(5);

        match &self.backend {
            SearchBackend::DuckDuckGo => {
                let html = self.fetch_ddg(&query)?;
                let results = parse_duckduckgo_results(&html, max_results as usize);
                Ok(ToolOutput::Success(serde_json::json!({
                    "query": query,
                    "max_results": max_results,
                    "results": results
                })))
            }
            SearchBackend::SearXNG { base_url } => {
                let results = self.fetch_searxng(base_url, &query, max_results as usize)?;
                Ok(ToolOutput::Success(serde_json::json!({
                    "query": query,
                    "max_results": max_results,
                    "results": results
                })))
            }
            SearchBackend::Brave { api_key } => {
                let results = self.fetch_brave(api_key, &query, max_results as usize)?;
                Ok(ToolOutput::Success(serde_json::json!({
                    "query": query,
                    "max_results": max_results,
                    "results": results
                })))
            }
        }
    }
}

impl WebSearchTool {
    /// Fetch results from DuckDuckGo HTML search with session warming.
    /// First visits the homepage to establish a session, then performs the search.
    fn fetch_ddg(&self, query: &str) -> Result<String, crate::tools::lib::ToolError> {
        let warmup_url = "https://duckduckgo.com/";
        let search_url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        // Step 1: Warm up session by visiting the homepage
        let _warmup = tokio::runtime::Handle::current()
            .block_on(async {
                self.http_client
                    .get(warmup_url)
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                    .header("Accept", "text/html")
                    .header("Accept-Language", "en-US,en;q=0.9,de;q=0.8")
                    .header("Accept-Encoding", "gzip, deflate, br")
                    .header("Connection", "keep-alive")
                    .send()
                    .await
            })
            .ok();

        // Small delay to simulate human behavior
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Step 2: Perform the search
        let html = tokio::runtime::Handle::current()
            .block_on(async {
                self.http_client
                    .get(&search_url)
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64")
                    .header("Accept", "text/html")
                    .header("Accept-Language", "en-US,en;q=0.9,de;q=0.8")
                    .header("Accept-Encoding", "identity")
                    .header("Connection", "keep-alive")
                    .header("Upgrade-Insecure-Requests", "1")
                    .send()
                    .await
                    .map_err(|e| crate::tools::lib::ToolError::Execution(format!("DDG HTTP request failed: {}", e)))?
                    .text()
                    .await
                    .map_err(|e| crate::tools::lib::ToolError::Execution(format!("Failed to read DDG response: {}", e)))
            })?;

        // Check if we got a bot challenge page instead of results
        if html.contains("anomaly-modal") || html.contains("Unfortunately, bots use DuckDuckGo") {
            return Err(crate::tools::lib::ToolError::Execution(
                "DuckDuckGo blocked the request (bot detection). Consider configuring a SearXNG backend or Brave Search API key."
                    .to_string(),
            ));
        }

        Ok(html)
    }

    /// Fetch results from a SearXNG instance (JSON API).
    fn fetch_searxng(
        &self,
        base_url: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<serde_json::Value>, crate::tools::lib::ToolError> {
        let url = format!(
            "{}/search?q={}&format=json",
            base_url.trim_end_matches('/'),
            urlencoding::encode(query)
        );

        let resp = tokio::runtime::Handle::current()
            .block_on(async {
                self.http_client
                    .get(&url)
                    .header("User-Agent", "WuffAgent/1.0")
                    .send()
                    .await
                    .map_err(|e| crate::tools::lib::ToolError::Execution(format!("SearXNG HTTP request failed: {}", e)))
            })?;

        if !resp.status().is_success() {
            return Err(crate::tools::lib::ToolError::Execution(
                format!("SearXNG returned status {}", resp.status()),
            ));
        }

        let body: serde_json::Value = tokio::runtime::Handle::current()
            .block_on(async {
                resp.text()
                    .await
                    .map_err(|e| crate::tools::lib::ToolError::Execution(format!("Failed to read SearXNG response: {}", e)))
            })?
            .parse()
            .map_err(|e| crate::tools::lib::ToolError::Execution(format!("Failed to parse SearXNG JSON: {}", e)))?;

        let results = body
            .get("results")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .take(max_results)
            .map(|r| {
                serde_json::json!({
                    "title": r.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "url": r.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "snippet": r.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                })
            })
            .collect();

        Ok(results)
    }

    /// Fetch results from Brave Search API.
    fn fetch_brave(
        &self,
        api_key: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<serde_json::Value>, crate::tools::lib::ToolError> {
        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            urlencoding::encode(query),
            max_results
        );

        let body = tokio::runtime::Handle::current()
            .block_on(async {
                self.http_client
                    .get(&url)
                    .header("User-Agent", "WuffAgent/1.0")
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Accept", "application/json")
                    .header("Accept-Encoding", "identity")
                    .send()
                    .await
                    .map_err(|e| crate::tools::lib::ToolError::Execution(format!("Brave HTTP request failed: {}", e)))?
                    .text()
                    .await
                    .map_err(|e| crate::tools::lib::ToolError::Execution(format!("Failed to read Brave response: {}", e)))
            })?;

        let resp: serde_json::Value = body
            .parse()
            .map_err(|e| crate::tools::lib::ToolError::Execution(format!("Failed to parse Brave JSON: {}", e)))?;

        let results = resp
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .take(max_results)
            .map(|r| {
                serde_json::json!({
                    "title": r.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "url": r.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "snippet": r.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                })
            })
            .collect();

        Ok(results)
    }
}

/// Minimal parser for DuckDuckGo HTML results.
fn parse_duckduckgo_results(html: &str, limit: usize) -> Vec<serde_json::Value> {
    let mut results = Vec::new();

    // DuckDuckGo HTML results use <article class="result"> elements
    let result_pattern = "<article";
    let rows: Vec<&str> = html.split(result_pattern).skip(1).collect();

    for row in rows.iter().take(limit) {
        // Extract title from <a class="result__a">
        let title = extract_between(row, "class=\"result__a\"", ">")
            .and_then(extract_text)
            .unwrap_or_default();

        // Extract URL from href attribute
        let url = extract_between(row, "href=\"", "\"")
            .or_else(|| extract_between(row, "href='", "'"))
            .map(clean_ddg_url)
            .unwrap_or_default();

        // Extract snippet from <div class="result__snippet">
        let snippet = extract_between(row, "class=\"result__snippet\"", ">")
            .and_then(extract_text)
            .unwrap_or_default();

        if !title.is_empty() {
            results.push(serde_json::json!({
                "title": title,
                "url": url,
                "snippet": snippet,
            }));
        }
    }

    results
}

/// Extract text between two markers.
fn extract_between<'a>(input: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let idx = input.find(start)? + start.len();
    let rest = &input[idx..];
    let end_idx = rest.find(end)?;
    Some(&rest[..end_idx])
}

/// Strip HTML entities to get plain text.
fn extract_text(input: &str) -> Option<String> {
    let stripped = html_unescape(input);
    Some(stripped.trim().to_string())
}

/// Simple HTML entity decoder.
fn html_unescape(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            let remaining: String = chars.by_ref().take(10).collect();
            match remaining.as_str() {
                "quot;" => { result.push('"'); continue; }
                "apos;" => { result.push('\''); continue; }
                "lt;" => { result.push('<'); continue; }
                "gt;" => { result.push('>'); continue; }
                "amp;" => { result.push('&'); continue; }
                "nbsp;" => { result.push(' '); continue; }
                _ => { result.push(c); }
            }
        }
        result.push(c);
    }
    result
}

/// DuckDuckGo wraps URLs in redirect links — extract the real URL.
fn clean_ddg_url(url: &str) -> String {
    if url.starts_with("/l/") && url.contains("uddg=") {
        if let Some(eq_pos) = url.find("uddg=") {
            let encoded = &url[eq_pos + 5..];
            let encoded = encoded.trim_end_matches(['"', '\'']);
            return match urlencoding::decode(encoded) {
                Ok(decoded) => decoded.into(),
                Err(_) => encoded.to_string(),
            };
        }
    }
    url.to_string()
}
