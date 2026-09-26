//! `update_memory`: refine an existing entry — new content, new tags, or
//! neither (a tag-only update keeps the existing text).

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::MemoryManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

use super::snippet;

/// Tool for updating existing memory entries.
pub struct UpdateMemoryTool {
    memory: Arc<MemoryManager>,
}

impl UpdateMemoryTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory }
    }
}

impl Tool for UpdateMemoryTool {
    fn name(&self) -> &str {
        "update_memory"
    }

    fn description(&self) -> &str {
        "Update an existing memory entry when information has changed or been refined. \
         Prefer this over save_memory when the fact already exists (use the ID from search_memory, \
         save_memory, or update_memory responses). Optionally replace the entry's tags. \
         `content` is OPTIONAL: when omitted (or null), the entry's existing content is kept \
         (useful for retagging or reviving an entry). Updating an entry revives it if it was \
         marked superseded."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "update_memory".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    ("id".to_string(), FieldSchema {
                        type_name: "string".to_string(),
                        description: "The memory entry ID".to_string(),
                        nullable: false,
                    }),
                    ("content".to_string(), FieldSchema {
                        type_name: "string".to_string(),
                        description: "Updated content (optional; omit or pass null to keep the existing content)".to_string(),
                        nullable: true,
                    }),
                    ("tags".to_string(), FieldSchema {
                        type_name: "array".to_string(),
                        description: "New tags replacing the old ones (optional; omit to keep current tags)".to_string(),
                        nullable: true,
                    }),
                ])),
                required: vec!["id".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let id = match params.get::<String>("id") {
            Some(i) if !i.is_empty() => i,
            _ => return Ok(ToolOutput::error("Memory ID is required")),
        };

        // `content` is optional: omitting it (or passing null / whitespace)
        // keeps the entry's existing content — handy for retagging or
        // reviving an entry without touching its text.
        let new_content: Option<String> = match params.get::<String>("content") {
            Some(c) if !c.trim().is_empty() => Some(c),
            _ => None,
        };

        let tags: Option<Vec<String>> = params.get::<Vec<String>>("tags");
        let tags = tags.filter(|t| !t.is_empty());

        match self.memory.update(&id, new_content.as_deref(), tags) {
            Ok(updated) => Ok(ToolOutput::success(format!(
                "Memory updated. id: {} [{}] tags: [{}] {}",
                updated.id,
                updated.r#type,
                updated.tags.join(", "),
                snippet(&updated.content),
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to update memory: {}", e))),
        }
    }
}
