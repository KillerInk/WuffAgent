/// Intelligent context trimming module.
///
/// Provides semantic-aware trimming of tool results and conversation
/// entries to reduce context bloat without losing important information.
///
/// ## Pipeline
///
/// 1. **Classify** — Detect content type (build log, source code, etc.)
/// 2. **Summarize** — Apply type-specific summarizer
/// 3. **Trigger** — Run at post-tool-call, post-agent-execution, and pre-save
///
/// ## Usage
///
/// ```rust
/// use wuffagent_core::trimming::ContextTrimming;
/// use wuffagent_core::trimming::config::TrimConfig;
///
/// let trimming = ContextTrimming::new();
/// let config = TrimConfig::default();
///
/// // Summarize a tool result
/// let summarized = trimming.summarize_tool_result(&result, &config);
///
/// // Inline summarize tool results in messages
/// trimming.inline_summarize(&mut messages, &config);
/// ```

pub mod classifier;
pub mod config;
pub mod summarizer;

pub use classifier::classify_content;
pub use config::TrimConfig;
pub use summarizer::ContextTrimming;
