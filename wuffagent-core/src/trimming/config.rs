/// Configuration for intelligent context trimming.
///
/// Controls thresholds and behavior for the trimming pipeline that
/// summarizes tool results, build logs, and other verbose outputs
/// to reduce context bloat.
// TrimConfig lives in the types brick (ChatToolPolicy embeds it, and
// ChatToolPolicy must live where AppEvent/QueuedMessage live); re-exported
// here so crate::trimming::config::TrimConfig and crate::trimming::TrimConfig
// stay stable.
pub use crate::types::TrimConfig;

#[cfg(test)]
mod tests;
