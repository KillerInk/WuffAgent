use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Search backend configuration.
///
/// Serde uses the PascalCase variant names (`"Auto"`, `"Bing"`, ...), which
/// keeps existing config files containing `"DuckDuckGo"` / `"SearXNG"` /
/// `"Brave"` deserializing unchanged.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SearchBackend {
    /// Failover chain: Bing → Yahoo → DuckDuckGo (default).
    Auto,
    /// Bing HTML search.
    Bing,
    /// Yahoo HTML search.
    Yahoo,
    /// DuckDuckGo HTML search.
    DuckDuckGo,
    /// Self-hosted SearXNG instance with JSON API.
    SearXNG { base_url: String },
    /// Brave Search API (compat only — never created by the UI, no env var).
    Brave { api_key: String },
}

impl Default for SearchBackend {
    fn default() -> Self {
        SearchBackend::Auto
    }
}

impl SearchBackend {
    /// Backends to try, in order. Auto = [Bing, Yahoo, DuckDuckGo]; anything
    /// else is just itself.
    pub fn chain(&self) -> Vec<SearchBackend> {
        match self {
            SearchBackend::Auto => vec![
                SearchBackend::Bing,
                SearchBackend::Yahoo,
                SearchBackend::DuckDuckGo,
            ],
            other => vec![other.clone()],
        }
    }

    /// Lowercase name for output JSON and cache keys:
    /// `"auto" | "bing" | "yahoo" | "duckduckgo" | "searxng" | "brave"`.
    pub fn label(&self) -> String {
        match self {
            SearchBackend::Auto => "auto".to_string(),
            SearchBackend::Bing => "bing".to_string(),
            SearchBackend::Yahoo => "yahoo".to_string(),
            SearchBackend::DuckDuckGo => "duckduckgo".to_string(),
            SearchBackend::SearXNG { .. } => "searxng".to_string(),
            SearchBackend::Brave { .. } => "brave".to_string(),
        }
    }

    /// Config backend with env-var overrides applied (handy for headless use):
    /// - `WUFFAGENT_SEARCH_BACKEND` = `auto|bing|yahoo|duckduckgo|searxng`
    ///   (PascalCase tolerated, unknown values ignored).
    /// - `WUFFAGENT_SEARXNG_URL` overrides the SearXNG `base_url` whenever the
    ///   resolved backend is SearXNG. Selecting `searxng` via the backend env
    ///   var without any URL (env or configured) keeps the configured backend.
    pub fn resolve_with_env(cfg: &SearchConfig) -> SearchBackend {
        let backend = match std::env::var("WUFFAGENT_SEARCH_BACKEND") {
            Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "auto" => SearchBackend::Auto,
                "bing" => SearchBackend::Bing,
                "yahoo" => SearchBackend::Yahoo,
                "duckduckgo" => SearchBackend::DuckDuckGo,
                "searxng" => match searxng_url_from_env_or_config(cfg) {
                    Some(url) => SearchBackend::SearXNG { base_url: url },
                    // No URL anywhere — keep the configured backend.
                    None => cfg.backend.clone(),
                },
                _ => cfg.backend.clone(),
            },
            Err(_) => cfg.backend.clone(),
        };

        // A configured/selected SearXNG backend picks up the env URL override.
        match &backend {
            SearchBackend::SearXNG { .. } => match std::env::var("WUFFAGENT_SEARXNG_URL") {
                Ok(url) if !url.trim().is_empty() => SearchBackend::SearXNG {
                    base_url: url.trim().to_string(),
                },
                _ => backend,
            },
            other => other.clone(),
        }
    }
}

/// SearXNG base URL from `WUFFAGENT_SEARXNG_URL`, falling back to the
/// configured SearXNG URL when it is non-empty.
fn searxng_url_from_env_or_config(cfg: &SearchConfig) -> Option<String> {
    if let Ok(url) = std::env::var("WUFFAGENT_SEARXNG_URL") {
        if !url.trim().is_empty() {
            return Some(url.trim().to_string());
        }
    }
    match &cfg.backend {
        SearchBackend::SearXNG { base_url } if !base_url.trim().is_empty() => {
            Some(base_url.trim().to_string())
        }
        _ => None,
    }
}

/// Region code for search results.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum SearchRegion {
    #[default]
    All,
    UsEn,
    UsDe,
    UkEn,
    UkDe,
    DeDe,
    DeEn,
    FrFr,
    FrEn,
    EsEs,
    EsEn,
    ItIt,
    ItEn,
    JpJa,
    JpEn,
    CnZh,
    CnEn,
    AuEn,
    CaEn,
    BrPt,
    InEn,
    InHi,
    KrKo,
    RuRu,
}

impl std::fmt::Display for SearchRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchRegion::All => write!(f, "wx-all"),
            SearchRegion::UsEn => write!(f, "us-en"),
            SearchRegion::UsDe => write!(f, "us-de"),
            SearchRegion::UkEn => write!(f, "uk-en"),
            SearchRegion::UkDe => write!(f, "uk-de"),
            SearchRegion::DeDe => write!(f, "de-de"),
            SearchRegion::DeEn => write!(f, "de-en"),
            SearchRegion::FrFr => write!(f, "fr-fr"),
            SearchRegion::FrEn => write!(f, "fr-en"),
            SearchRegion::EsEs => write!(f, "es-es"),
            SearchRegion::EsEn => write!(f, "es-en"),
            SearchRegion::ItIt => write!(f, "it-it"),
            SearchRegion::ItEn => write!(f, "it-en"),
            SearchRegion::JpJa => write!(f, "jp-jp"),
            SearchRegion::JpEn => write!(f, "jp-en"),
            SearchRegion::CnZh => write!(f, "cn-zh"),
            SearchRegion::CnEn => write!(f, "cn-en"),
            SearchRegion::AuEn => write!(f, "au-en"),
            SearchRegion::CaEn => write!(f, "ca-en"),
            SearchRegion::BrPt => write!(f, "br-pt"),
            SearchRegion::InEn => write!(f, "in-en"),
            SearchRegion::InHi => write!(f, "in-hi"),
            SearchRegion::KrKo => write!(f, "kr-ko"),
            SearchRegion::RuRu => write!(f, "ru-ru"),
        }
    }
}

/// Time range for search results.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum TimeRange {
    #[default]
    All,
    PastDay,
    PastWeek,
    PastMonth,
    PastYear,
}

impl std::fmt::Display for TimeRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimeRange::All => write!(f, ""),
            TimeRange::PastDay => write!(f, "d"),
            TimeRange::PastWeek => write!(f, "w"),
            TimeRange::PastMonth => write!(f, "m"),
            TimeRange::PastYear => write!(f, "y"),
        }
    }
}

/// Configuration for web search functionality.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchConfig {
    /// The search backend to use.
    #[serde(default)]
    pub backend: SearchBackend,
    /// Default region for search results.
    #[serde(default)]
    pub region: SearchRegion,
    /// Default time range filter.
    #[serde(default)]
    pub time_range: TimeRange,
    /// Maximum number of results to return per query.
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    /// Cache duration in seconds (default: 5 minutes).
    #[serde(default = "default_cache_duration")]
    pub cache_duration_secs: u64,
    /// Custom headers to include in search requests.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub custom_headers: HashMap<String, String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::Auto,
            region: SearchRegion::All,
            time_range: TimeRange::All,
            max_results: 10,
            cache_duration_secs: 300,
            custom_headers: HashMap::new(),
        }
    }
}

impl SearchConfig {
    /// Create a new search config with the given backend.
    pub fn with_backend(mut self, backend: SearchBackend) -> Self {
        self.backend = backend;
        self
    }

    /// Create a new search config with the given region.
    pub fn with_region(mut self, region: SearchRegion) -> Self {
        self.region = region;
        self
    }

    /// Create a new search config with the given time range.
    pub fn with_time_range(mut self, time_range: TimeRange) -> Self {
        self.time_range = time_range;
        self
    }

    /// Create a new search config with the given max results.
    pub fn with_max_results(mut self, max_results: u32) -> Self {
        self.max_results = max_results.min(20).max(1);
        self
    }

    /// Create a new search config with the given cache duration.
    pub fn with_cache_duration(mut self, cache_duration_secs: u64) -> Self {
        self.cache_duration_secs = cache_duration_secs;
        self
    }

    /// Add a custom header to the search requests.
    pub fn with_custom_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_headers.insert(key.into(), value.into());
        self
    }
}

fn default_max_results() -> u32 {
    10
}

fn default_cache_duration() -> u64 {
    300 // 5 minutes
}
#[cfg(test)]
mod tests;
