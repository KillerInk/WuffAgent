//! `search_memory`: find relevant entries (with IDs for update/delete/
//! consolidate).

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::MemoryManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

/// Tool for searching relevant memories.
pub struct SearchMemoryTool {
    memory: Arc<MemoryManager>,
}

impl SearchMemoryTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory }
    }
}

impl Tool for SearchMemoryTool {
    fn name(&self) -> &str {
        "search_memory"
    }

    fn description(&self) -> &str {
        "Search project memory for relevant information before starting work or before saving a \
         new memory. Results include entry IDs you can pass to update_memory, delete_memory, or \
         consolidate_memories."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "search_memory".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([(
                    "query".to_string(),
                    FieldSchema {
                        type_name: "string".to_string(),
                        description: "Search query".to_string(),
                        nullable: false,
                    },
                )])),
                required: vec!["query".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let query = match params.get::<String>("query") {
            Some(q) if !q.trim().is_empty() => q,
            _ => return Ok(ToolOutput::error("Search query is required")),
        };

        let results = self.memory.search(&query);

        if results.is_empty() {
            return Ok(ToolOutput::success("No relevant memories found"));
        }

        let mut output = String::from(format!("Found {} relevant memory(ies):\n", results.len()));
        for mem in &results {
            output.push_str(&format!(
                "- [{}] (id: {}) {}\n",
                mem.r#type, mem.id, mem.content
            ));
        }

        Ok(ToolOutput::success(output))
    }
}
