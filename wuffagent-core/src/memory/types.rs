use serde::{Deserialize, Serialize};
use chrono::{Utc, DateTime};

/// Types of memories that can be stored.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum MemoryType {
    #[serde(rename = "fact")]
    #[default]
    Fact,
    #[serde(rename = "lesson")]
    Lesson,
    #[serde(rename = "decision")]
    Decision,
    #[serde(rename = "context")]
    Context,
    #[serde(rename = "goal")]
    Goal,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::Fact => write!(f, "FACT"),
            MemoryType::Lesson => write!(f, "LESSON"),
            MemoryType::Decision => write!(f, "DECISION"),
            MemoryType::Context => write!(f, "CONTEXT"),
            MemoryType::Goal => write!(f, "GOAL"),
        }
    }
}

/// A single memory entry.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemoryEntry {
    pub id: String,
    #[serde(default)]
    pub r#type: MemoryType,
    pub content: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, with = "chrono::serde::ts_seconds_option")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// If Some, this entry supersedes older entries (marked during summarization).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

impl MemoryEntry {
    pub fn new(r#type: MemoryType, content: &str, source: &str, tags: &[&str]) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            r#type,
            content: content.to_string(),
            source: source.to_string(),
            timestamp: Some(Utc::now()),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            confidence: 1.0,
            session_id: None,
            project: None,
            supersedes: None,
        }
    }

    pub fn is_expired(&self) -> bool {
        // Entries older than 90 days are considered expired
        if let Some(ts) = self.timestamp {
            Utc::now().signed_duration_since(ts).num_days() > 90
        } else {
            false
        }
    }
}

fn default_confidence() -> f32 { 1.0 }

/// Search mode for memory retrieval.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[serde(rename = "keyword")]
    #[default]
    Keyword,
}

/// Deserialize a search mode, mapping the legacy `"llm"` value to `Keyword`
/// so existing config files keep loading after the dead variant was removed.
pub fn deserialize_search_mode<'de, D>(deserializer: D) -> Result<SearchMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    match value.to_lowercase().as_str() {
        "keyword" | "llm" => Ok(SearchMode::Keyword),
        other => Err(serde::de::Error::unknown_variant(other, &["keyword"])),
    }
}

impl std::fmt::Display for SearchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchMode::Keyword => write!(f, "keyword"),
        }
    }
}

/// Injection mode for memory context.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum InjectionMode {
    #[serde(rename = "always")]
    Always,
    #[serde(rename = "smart")]
    #[default]
    Smart,
    #[serde(rename = "off")]
    Off,
}

impl std::fmt::Display for InjectionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InjectionMode::Always => write!(f, "always"),
            InjectionMode::Smart => write!(f, "smart"),
            InjectionMode::Off => write!(f, "off"),
        }
    }
}

/// Configuration for the memory system.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemoryConfig {
    /// Whether memory is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Maximum number of memories to store before eviction.
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
    /// Maximum memories to inject into system prompts.
    #[serde(default = "default_injection_max_entries")]
    pub injection_max_entries: usize,
    /// Maximum characters for injected memory block.
    #[serde(default = "default_injection_max_chars")]
    pub injection_max_chars: usize,
    /// Search mode.
    #[serde(default, deserialize_with = "deserialize_search_mode")]
    pub search_mode: SearchMode,
    /// Injection mode.
    #[serde(default)]
    pub injection_mode: InjectionMode,
    /// Project name for memory scoping.
    #[serde(default = "default_project")]
    pub project: String,
    /// Path to memories directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memories_dir: Option<String>,
    /// Whether to auto-suggest agent prompt improvements.
    #[serde(default = "default_auto_improve")]
    pub auto_improve: bool,
    /// Minimum number of relevant lessons to trigger improvement check.
    #[serde(default = "default_improvement_trigger_lessons")]
    pub improvement_trigger_lessons: usize,
    /// Whether the LLM memory-maintenance pass is enabled (opt-in).
    #[serde(default = "default_memory_maintenance")]
    pub memory_maintenance: bool,
    /// Run the maintenance pass only when active entries reach this count.
    #[serde(default = "default_memory_maintenance_threshold")]
    pub memory_maintenance_threshold: usize,
    /// How long ONE maintenance step (a single batch / LLM call) may run
    /// (seconds) before it is cancelled. Maintenance runs the store in
    /// batches of `memory_maintenance_batch_size`, so this bounds each step,
    /// not the whole pass. Local LLMs can be slow, so the default is
    /// generous.
    #[serde(default = "default_memory_maintenance_timeout_secs")]
    pub memory_maintenance_timeout_secs: u64,
    /// Number of entries per maintenance batch (one small LLM call each).
    /// Clamped to 5..=100 at use site.
    #[serde(default = "default_memory_maintenance_batch_size")]
    pub memory_maintenance_batch_size: usize,
}

fn default_enabled() -> bool { true }
fn default_max_entries() -> usize { 100 }
fn default_injection_max_entries() -> usize { 5 }
fn default_injection_max_chars() -> usize { 1000 }
fn default_project() -> String { "default".to_string() }
fn default_auto_improve() -> bool { false }
fn default_improvement_trigger_lessons() -> usize { 1 }
fn default_memory_maintenance() -> bool { false }
fn default_memory_maintenance_threshold() -> usize { 40 }
fn default_memory_maintenance_timeout_secs() -> u64 { 600 }
fn default_memory_maintenance_batch_size() -> usize { 15 }

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 100,
            injection_max_entries: 5,
            injection_max_chars: 1000,
            search_mode: SearchMode::default(),
            injection_mode: InjectionMode::Smart,
            project: "default".to_string(),
            memories_dir: None,
            auto_improve: false,
            improvement_trigger_lessons: 1,
            memory_maintenance: false,
            memory_maintenance_threshold: 40,
            memory_maintenance_timeout_secs: 600,
            memory_maintenance_batch_size: 15,
        }
    }
}

#[cfg(test)]
mod tests;
