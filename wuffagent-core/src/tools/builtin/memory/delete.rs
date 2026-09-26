//! `delete_memory`: remove a single entry by ID.

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::MemoryManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

use super::snippet;

/// Tool for deleting a memory entry.
pub struct DeleteMemoryTool {
    memory: Arc<MemoryManager>,
}

impl DeleteMemoryTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory }
    }
}

impl Tool for DeleteMemoryTool {
    fn name(&self) -> &str {
        "delete_memory"
    }

    fn description(&self) -> &str {
        "Delete a memory entry by ID. Use for entries that are stale, wrong, or fully covered by a \
         better entry (see search_memory / save_memory responses for IDs). Prefer update_memory or \
         consolidate_memories when the information is still useful in a refined form."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_memory".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([(
                    "id".to_string(),
                    FieldSchema {
                        type_name: "string".to_string(),
                        description: "The memory entry ID to delete".to_string(),
                        nullable: false,
                    },
                )])),
                required: vec!["id".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let id = match params.get::<String>("id") {
            Some(i) if !i.is_empty() => i,
            _ => return Ok(ToolOutput::error("Memory ID is required")),
        };

        match self.memory.delete(&id) {
            Ok(Some(removed)) => Ok(ToolOutput::success(format!(
                "Memory deleted. id: {} [{}] {}",
                removed.id,
                removed.r#type,
                snippet(&removed.content),
            ))),
            Ok(None) => Ok(ToolOutput::error(format!(
                "Memory '{}' not found. Use search_memory to find the correct ID.",
                id
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to delete memory: {}", e))),
        }
    }
}
