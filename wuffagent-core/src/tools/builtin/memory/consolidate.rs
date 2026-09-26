//! `consolidate_memories`: merge related entries into one (the originals
//! are removed; missing IDs are reported, not fatal).

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::{MemoryAddResult, MemoryEntry, MemoryManager, MemoryType};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

use super::snippet;

/// Tool for consolidating related memories.
pub struct ConsolidateMemoriesTool {
    memory: Arc<MemoryManager>,
}

impl ConsolidateMemoriesTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory }
    }
}

impl Tool for ConsolidateMemoriesTool {
    fn name(&self) -> &str {
        "consolidate_memories"
    }

    fn description(&self) -> &str {
        "Merge multiple related memories into a single, more comprehensive entry. \
         Pass the entry IDs (from search_memory / save_memory) plus the merged content. \
         The original entries are removed and the response returns the new entry's ID."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "consolidate_memories".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "memory_ids".to_string(),
                        FieldSchema {
                            type_name: "array".to_string(),
                            description: "IDs of memories to merge".to_string(),
                            nullable: false,
                        },
                    ),
                    (
                        "consolidated_content".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "The merged, comprehensive content".to_string(),
                            nullable: false,
                        },
                    ),
                ])),
                required: vec!["memory_ids".to_string(), "consolidated_content".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let memory_ids = match params.get::<Vec<String>>("memory_ids") {
            Some(ids) if !ids.is_empty() => ids,
            _ => return Ok(ToolOutput::error("At least one memory ID is required")),
        };

        let content = match params.get::<String>("consolidated_content") {
            Some(c) if !c.trim().is_empty() => c,
            _ => return Ok(ToolOutput::error("Consolidated content cannot be empty")),
        };

        // Delete the old memories
        let mut deleted = 0;
        let mut missing = Vec::new();
        for id in &memory_ids {
            match self.memory.delete(id) {
                Ok(Some(_)) => deleted += 1,
                Ok(None) => missing.push(id.clone()),
                Err(e) => {
                    return Ok(ToolOutput::error(format!(
                        "Failed to delete memory '{}': {}",
                        id, e
                    )))
                }
            }
        }

        // Create the consolidated memory
        let entry = MemoryEntry::new(MemoryType::Fact, &content, "agent", &["consolidated"]);
        match self.memory.add(entry) {
            Ok(MemoryAddResult::Created(created)) => {
                let mut msg = format!(
                    "Consolidated {} memories into a single entry. id: {} [{}] {}",
                    deleted,
                    created.id,
                    created.r#type,
                    snippet(&created.content),
                );
                if !missing.is_empty() {
                    msg.push_str(&format!("\nWarning: {} ID(s) not found and were skipped: {}", missing.len(), missing.join(", ")));
                }
                Ok(ToolOutput::success(msg))
            }
            Ok(MemoryAddResult::Duplicate(existing)) => Ok(ToolOutput::success(format!(
                "Consolidated {} memories, but the result resembles an existing entry (id: {} [{}] {}). \
                 The merged content was not stored; consider update_memory on that ID instead.",
                deleted,
                existing.id,
                existing.r#type,
                snippet(&existing.content),
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to save consolidated memory: {}", e))),
        }
    }
}
