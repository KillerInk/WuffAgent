use std::path::{Path, PathBuf};
use std::fs;
use tracing;

use super::types::{MemoryEntry, MemoryConfig};

/// Load memories from a JSON file.
/// Backwards-compatible with the old format that used plain objects without
/// confidence/session_id/project/supersedes fields.
pub fn load_memories(path: &Path) -> Result<Vec<MemoryEntry>, String> {
    tracing::debug!("[MEMORY] Loading memories from {:?}", path);
    if !path.exists() {
        tracing::debug!("[MEMORY] Memory file does not exist, starting fresh: {:?}", path);
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read memory file {:?}: {}", path, e))?;

    let content = content.trim();
    if content.is_empty() {
        return Ok(Vec::new());
    }

    // Try parsing as array first
    if let Ok(entries) = serde_json::from_str::<Vec<MemoryEntry>>(content) {
        tracing::info!("Loaded {} memories from {:?}", entries.len(), path);
        return Ok(entries);
    }

    // Try parsing as single object
    if let Ok(entry) = serde_json::from_str::<MemoryEntry>(content) {
        tracing::info!("Loaded 1 memory from {:?}", path);
        return Ok(vec![entry]);
    }

    Err(format!("Failed to parse memory file {:?}: invalid JSON", path))
}

/// Save memories to a JSON file atomically.
pub fn save_memories(path: &Path, entries: &[MemoryEntry]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create memories directory {:?}: {}", parent, e))?;
    }

    let temp_path = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(entries)
        .map_err(|e| format!("Failed to serialize memories: {}", e))?;

    fs::write(&temp_path, content)
        .map_err(|e| format!("Failed to write memory file {:?}: {}", temp_path, e))?;

    fs::rename(&temp_path, path)
        .map_err(|e| format!("Failed to rename temp file to {:?}: {}", path, e))?;

    tracing::debug!("[MEMORY] Saved {} memories to {:?}", entries.len(), path);
    Ok(())
}

/// Get the path to the project memories file.
pub fn get_memories_path(config: &MemoryConfig) -> PathBuf {
    let memories_dir = if let Some(dir) = &config.memories_dir {
        PathBuf::from(dir)
    } else {
        // Default: ~/.wuffcode/memories
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".wuffcode")
            .join("memories")
    };
    let path = memories_dir.join(format!("{}.json", config.project));
    tracing::debug!("[MEMORY] Memory storage path: {:?}", path);
    path
}

/// Count active (non-expired, non-superseded) memories.
pub fn count_active_memories(entries: &[MemoryEntry]) -> usize {
    entries.iter()
        .filter(|e| !e.is_expired() && e.supersedes.is_none())
        .count()
}