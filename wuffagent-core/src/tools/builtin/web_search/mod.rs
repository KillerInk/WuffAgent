use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::SearchBackend;
use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

mod parsers;

use parsers::*;

/// Desktop browser user agent for the HTML search backends.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Per-backend request timeout (failover chains must stay snappy).
const BACKEND_TIMEOUT: Duration = Duration::from_secs(12);

/// A tool that searches the web via a configurable keyless search endpoint.
///
/// `backend` is `Auto` by default, which runs the failover chain
/// Bing → Yahoo → DuckDuckGo and reports the backend that actually answered
/// in the output JSON.
pub struct WebSearchTool {
    http_client: reqwest::Client,
    backend: SearchBackend,
    /// Default `max_results` when the caller omits the param (config, clamped 1..=20).
    default_max_results: u32,
    /// TTL for the in-memory result cache (`search.cache_duration_secs`).
    cache_ttl: Duration,
}

/// Shared reqwest client used across all WebSearchTool instances for connection pooling.
fn shared_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(4)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client builder")
}

/// Cached tokio current-thread runtime for use inside spawn_blocking calls.
/// Avoids creating a new runtime on every web search invocation.
static BLOCKING_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build blocking runtime")
});

/// Run an async block to completion. Reuses the ambient runtime handle when
/// available (e.g. inside `spawn_blocking`), otherwise the cached
/// `BLOCKING_RUNTIME`. Used by ALL fetch functions (audit finding B8: the
/// SearXNG/Brave fetchers used to build a fresh runtime per call).
macro_rules! block_on {
    ($expr:expr) => {{
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on($expr),
            Err(_) => BLOCKING_RUNTIME.block_on($expr),
        }
    }};
}

// ─── Result cache ────────────────────────────────────────────────────────────

struct CacheEntry {
    at: Instant,
    results: Vec<Value>,
    /// Empty results from a successful fetch (challenge page / no organic
    /// items) are cached too, so a blocked backend doesn't hammer the network
    /// for the TTL window. Network errors are NOT cached (transient).
    blocked: bool,
}

static RESULT_CACHE: LazyLock<std::sync::Mutex<HashMap<(String, String), CacheEntry>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn cache_get(label: &str, query: &str, ttl: Duration) -> Option<(Vec<Value>, bool)> {
    let map = RESULT_CACHE.lock().unwrap();
    map.get(&(label.to_string(), query.to_string()))
        .filter(|e| e.at.elapsed() < ttl)
        .map(|e| (e.results.clone(), e.blocked))
}

fn cache_put(label: &str, query: &str, results: Vec<Value>, blocked: bool) {
    let mut map = RESULT_CACHE.lock().unwrap();
    map.insert(
        (label.to_string(), query.to_string()),
        CacheEntry {
            at: Instant::now(),
            results,
            blocked,
        },
    );
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            http_client: shared_client(),
            backend: SearchBackend::Auto,
            default_max_results: 10,
            cache_ttl: Duration::from_secs(300),
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

    pub fn with_default_max_results(mut self, max_results: u32) -> Self {
        self.default_max_results = max_results.min(20).max(1);
        self
    }

    pub fn with_cache_ttl(mut self, cache_ttl: Duration) -> Self {
        self.cache_ttl = cache_ttl;
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
        "Search the web for information using a search engine (Bing/Yahoo/DuckDuckGo with automatic failover, or SearXNG)"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".to_string(),
            description: "Search the web for information".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "query".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Search query".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "max_results".to_string(),
                        crate::tools::types::FieldSchema {
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

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let query: String = params.get("query").ok_or_else(|| {
            crate::tools::types::ToolError::InvalidParams("query is required".to_string())
        })?;

        let max_results: u32 = params
            .get("max_results")
            .unwrap_or(self.default_max_results)
            .min(20)
            .max(1);
        let max = max_results as usize;

        // Explicit backends have a chain of one; Auto expands to the
        // failover list. Per-backend errors are String so the chain can
        // accumulate one reason per backend.
        let chain = self.backend.chain();
        let fetch = |backend: &SearchBackend| -> Result<Vec<Value>, String> {
            match backend {
                SearchBackend::Bing => self.fetch_bing(&query, max),
                SearchBackend::Yahoo => self.fetch_yahoo(&query, max),
                SearchBackend::DuckDuckGo => self.fetch_ddg(&query, max),
                SearchBackend::SearXNG { base_url } => self.fetch_searxng(base_url, &query, max),
                SearchBackend::Brave { api_key } => self.fetch_brave(api_key, &query, max),
                SearchBackend::Auto => unreachable!("chain() never contains Auto"),
            }
        };

        let (backend, results) = self.run_chain(&chain, &query, fetch).map_err(|failures| {
            crate::tools::types::ToolError::Execution(format!(
                "All search backends failed: {}",
                failures.join("; ")
            ))
        })?;

        Ok(ToolOutput::Success(serde_json::json!({
            "query": query,
            "max_results": max_results,
            "backend": backend.label(),
            "results": results
        })))
    }
}

impl WebSearchTool {
    /// Run the backend chain: first backend returning ≥1 result wins.
    ///
    /// - Fresh cache entry → used without a network call (empty + blocked
    ///   counts as blocked and fails through).
    /// - `Ok(results)` non-empty → success (cached).
    /// - `Ok([])` → "no results (blocked)" (cached), continue.
    /// - `Err(e)` → recorded, continue (not cached).
    /// All backends failed → `Err` with one reason string per backend.
    fn run_chain<F>(
        &self,
        chain: &[SearchBackend],
        query: &str,
        mut fetch: F,
    ) -> Result<(SearchBackend, Vec<Value>), Vec<String>>
    where
        F: FnMut(&SearchBackend) -> Result<Vec<Value>, String>,
    {
        let mut failures: Vec<String> = Vec::new();

        for backend in chain {
            let label = backend.label();

            if let Some((results, blocked)) = cache_get(&label, query, self.cache_ttl) {
                if blocked || results.is_empty() {
                    failures.push(format!("{label}: no results (blocked, cached)"));
                    continue;
                }
                return Ok((backend.clone(), results));
            }

            match fetch(backend) {
                Ok(results) if !results.is_empty() => {
                    cache_put(&label, query, results.clone(), false);
                    return Ok((backend.clone(), results));
                }
                Ok(_) => {
                    cache_put(&label, query, Vec::new(), true);
                    failures.push(format!("{label}: no results (blocked)"));
                }
                Err(e) => {
                    failures.push(format!("{label}: {e}"));
                }
            }
        }

        Err(failures)
    }

    /// Fetch results from Bing HTML search.
    fn fetch_bing(&self, query: &str, max_results: usize) -> Result<Vec<Value>, String> {
        let html_body = block_on!(async {
            self.http_client
                .get("https://www.bing.com/search")
                .query(&[("q", query), ("count", &max_results.to_string())])
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "text/html")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Accept-Encoding", "identity")
                .timeout(BACKEND_TIMEOUT)
                .send()
                .await
                .map_err(|e| format!("Bing HTTP request failed: {e}"))?
                .text()
                .await
                .map_err(|e| format!("Failed to read Bing response: {e}"))
        })?;

        Ok(parse_bing_results(&html_body, max_results))
    }

    /// Fetch results from Yahoo HTML search.
    fn fetch_yahoo(&self, query: &str, max_results: usize) -> Result<Vec<Value>, String> {
        let html_body = block_on!(async {
            self.http_client
                .get("https://search.yahoo.com/search")
                .query(&[("p", query), ("n", &max_results.to_string())])
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "text/html")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Accept-Encoding", "identity")
                .timeout(BACKEND_TIMEOUT)
                .send()
                .await
                .map_err(|e| format!("Yahoo HTTP request failed: {e}"))?
                .text()
                .await
                .map_err(|e| format!("Failed to read Yahoo response: {e}"))
        })?;

        Ok(parse_yahoo_results(&html_body, max_results))
    }

    /// Fetch results from DuckDuckGo HTML search. Kept in the failover
    /// chain because it works from some IPs; a bot-challenge page is
    /// detected and reported as an error so the chain falls through.
    fn fetch_ddg(&self, query: &str, max_results: usize) -> Result<Vec<Value>, String> {
        let html_body = block_on!(async {
            self.http_client
                .get("https://html.duckduckgo.com/html/")
                .query(&[("q", query)])
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "text/html")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Accept-Encoding", "identity")
                .timeout(BACKEND_TIMEOUT)
                .send()
                .await
                .map_err(|e| format!("DDG HTTP request failed: {e}"))?
                .text()
                .await
                .map_err(|e| format!("Failed to read DDG response: {e}"))
        })?;

        // Check if we got a bot challenge page instead of results.
        if html_body.contains("anomaly-modal")
            || html_body.contains("Unfortunately, bots use DuckDuckGo")
        {
            return Err("blocked by bot detection".to_string());
        }

        Ok(parse_duckduckgo_results(&html_body, max_results))
    }

    /// Fetch results from a SearXNG instance (JSON API).
    fn fetch_searxng(
        &self,
        base_url: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<Value>, String> {
        let resp = block_on!(async {
            self.http_client
                .get(format!("{}/search", base_url.trim_end_matches('/')))
                .query(&[("q", query), ("format", "json")])
                .header("User-Agent", "WuffAgent/1.0")
                .timeout(BACKEND_TIMEOUT)
                .send()
                .await
                .map_err(|e| format!("SearXNG HTTP request failed: {e}"))
        })?;

        if !resp.status().is_success() {
            return Err(format!("SearXNG returned status {}", resp.status()));
        }

        let body: Value = block_on!(async {
            resp.text()
                .await
                .map_err(|e| format!("Failed to read SearXNG response: {e}"))
        })?
        .parse()
        .map_err(|e| format!("Failed to parse SearXNG JSON: {e}"))?;

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

    /// Fetch results from Brave Search API (compat backend, requires key).
    fn fetch_brave(
        &self,
        api_key: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<Value>, String> {
        let body = block_on!(async {
            self.http_client
                .get("https://api.search.brave.com/res/v1/web/search")
                .query(&[("q", query), ("count", &max_results.to_string())])
                .header("User-Agent", "WuffAgent/1.0")
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Accept", "application/json")
                .header("Accept-Encoding", "identity")
                .timeout(BACKEND_TIMEOUT)
                .send()
                .await
                .map_err(|e| format!("Brave HTTP request failed: {e}"))?
                .text()
                .await
                .map_err(|e| format!("Failed to read Brave response: {e}"))
        })?;

        let resp: Value = body
            .parse()
            .map_err(|e| format!("Failed to parse Brave JSON: {e}"))?;

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

// ─── Tests ───────────────────────────────────────────────────────────────────
//
// The search parsers are tested OFFLINE against saved HTML fixtures in
// `wuffagent-core/tests/fixtures/` (bing/yahoo: real result pages; ddg: the
// bot-challenge page). Keep the fixtures current when provider markup changes.

#[cfg(test)]
mod tests;
