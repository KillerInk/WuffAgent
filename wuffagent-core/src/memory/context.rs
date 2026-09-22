//! Memory context-block rendering for system-prompt injection
//! (build_context_block and its private helpers). Pure code motion from
//! manager.rs.

use super::manager::MemoryManager;
use super::types::MemoryEntry;

impl MemoryManager {
    /// Build the memory context block for injection into system prompts.
    pub fn build_context_block(&self, query: &str) -> String {
        let config = self.config();
        if !config.enabled || config.injection_mode == super::types::InjectionMode::Off {
            return String::new();
        }

        let memories = if query.trim().is_empty() {
            // No query available: fall back to the most recent entries.
            self.get_recent(config.injection_max_entries)
        } else {
            let results = self.search(query);
            if results.is_empty() {
                match config.injection_mode {
                    // Smart mode: inject only when there are relevant hits.
                    super::types::InjectionMode::Smart => return String::new(),
                    // Always mode: fall back to the 3 most recent entries.
                    super::types::InjectionMode::Always => return self.fallback_recent_block(3),
                    super::types::InjectionMode::Off => unreachable!(),
                }
            }
            results
        };
        self.render_block(memories)
    }

    /// Render a block from the given entries (shared by the main path and the
    /// always-mode recent fallback).
    fn render_block(&self, memories: Vec<MemoryEntry>) -> String {
        if memories.is_empty() {
            return String::new();
        }

        let max_chars = self.config().injection_max_chars;
        let mut block =
            String::from("\n═══ MEMORY CONTEXT ═══\n(Relevant memories from past sessions)\n\n");
        let mut chars = 0;

        for memory in &memories {
            let line = format!("[{}] {}\n", memory.r#type, memory.content);
            if chars + line.len() > max_chars {
                break;
            }
            block.push_str(&line);
            chars += line.len();
        }

        block.push_str("══════════════════════\n");
        block
    }

    /// Always-mode fallback block built from the N most recent entries.
    fn fallback_recent_block(&self, count: usize) -> String {
        self.render_block(self.get_recent(count))
    }
}
