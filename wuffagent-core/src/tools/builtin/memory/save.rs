//! `save_memory`: store a new memory entry (quality gate, dedup feedback,
//! related-entry hints for consolidation).

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::{MemoryAddResult, MemoryEntry, MemoryManager};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

use super::snippet;

/// Tool for saving persistent memory entries.
/// Use this when you discover something non-obvious that will be useful in future sessions.
pub struct SaveMemoryTool {
    memory: Arc<MemoryManager>,
}

impl SaveMemoryTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory }
    }
}

impl Tool for SaveMemoryTool {
    fn name(&self) -> &str {
        "save_memory"
    }

    fn description(&self) -> &str {
        "Save a persistent fact, lesson, or decision to project memory. Use when discovering \
         non-obvious information that will be useful in future sessions. Only save facts that \
         are persistent (project architecture, known issues, patterns) - not ephemeral information. \
         Before saving, search_memory first; if an entry already covers this, use update_memory or \
         consolidate_memories instead of re-adding. The response returns the new entry ID and \
         related existing entries so you can consolidate later."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "save_memory".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "type".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Memory type: fact, lesson, decision, context, or goal"
                                .to_string(),
                            nullable: false,
                        },
                    ),
                    (
                        "content".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description:
                                "The memory content (1-3 sentences, specific and complete)"
                                    .to_string(),
                            nullable: false,
                        },
                    ),
                    (
                        "tags".to_string(),
                        FieldSchema {
                            type_name: "array".to_string(),
                            description:
                                "Tags for categorization (optional). Tag lessons with your \
                                      agent name (e.g. \"agent:coder\") so per-agent improvement \
                                      checks can find them"
                                    .to_string(),
                            nullable: true,
                        },
                    ),
                ])),
                required: vec!["type".to_string(), "content".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let content = match params.get::<String>("content") {
            Some(c) if !c.trim().is_empty() => c,
            _ => return Ok(ToolOutput::error("Memory content cannot be empty")),
        };

        // Quality check: minimum meaningful length
        if content.split_whitespace().count() < 10 {
            return Ok(ToolOutput::error("Memory too short to be useful. Provide specific, complete information (at least 10 words)."));
        }

        // Determine memory type
        let type_str = params
            .get::<String>("type")
            .unwrap_or_else(|| "fact".to_string());
        let mem_type = MemoryManager::parse_memory_type(&type_str);

        // Parse tags
        let tags: Vec<String> = params.get::<Vec<String>>("tags").unwrap_or_default();
        let tags: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();

        // Create and save the memory
        let entry = MemoryEntry::new(mem_type.clone(), &content, "agent", &tags);
        match self.memory.add(entry) {
            Ok(MemoryAddResult::Created(created)) => {
                // Related existing entries (top 3) so the agent can consolidate when relevant.
                let related = self.memory.search(&created.content);
                let related = related
                    .iter()
                    .filter(|e| e.id != created.id)
                    .take(3)
                    .map(|e| format!("  - id: {} [{}] {}", e.id, e.r#type, snippet(&e.content)))
                    .collect::<Vec<_>>()
                    .join("\n");

                let mut msg = format!(
                    "Memory saved. id: {} [{}] {}\nUse update_memory (by ID) to refine it, or \
                     consolidate_memories to merge it with related entries.",
                    created.id,
                    created.r#type,
                    snippet(&created.content),
                );
                if !related.is_empty() {
                    msg.push_str("\nRelated existing memories:\n");
                    msg.push_str(&related);
                }
                Ok(ToolOutput::success(msg))
            }
            Ok(MemoryAddResult::Duplicate(existing)) => Ok(ToolOutput::success(format!(
                "Not saved: a near-duplicate already exists.\n\
                 Existing entry id: {} [{}] {}\n\
                 Use update_memory (id: {}) to refine it, or consolidate_memories to merge the new \
                 information into it, or delete_memory if it is stale.",
                existing.id,
                existing.r#type,
                snippet(&existing.content),
                existing.id,
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to save memory: {}", e))),
        }
    }
}
