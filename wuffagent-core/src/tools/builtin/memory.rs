use std::sync::Arc;
use crate::memory::{MemoryManager, MemoryEntry, MemoryType};
use crate::tools::types::{Tool, ToolParams, ToolOutput, ToolResult, ToolSchema, JsonSchema, FieldSchema};
use std::collections::HashMap;

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
    fn name(&self) -> &str { "save_memory" }

    fn description(&self) -> &str {
        "Save a persistent fact, lesson, or decision to project memory. Use when discovering \
         non-obvious information that will be useful in future sessions. Only save facts that \
         are persistent (project architecture, known issues, patterns) - not ephemeral information."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "save_memory".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    ("type".to_string(), FieldSchema {
                        type_name: "string".to_string(),
                        description: "Memory type: fact, lesson, decision, context, or goal".to_string(),
                        nullable: false,
                    }),
                    ("content".to_string(), FieldSchema {
                        type_name: "string".to_string(),
                        description: "The memory content (1-3 sentences, specific and complete)".to_string(),
                        nullable: false,
                    }),
                    ("tags".to_string(), FieldSchema {
                        type_name: "array".to_string(),
                        description: "Tags for categorization (optional)".to_string(),
                        nullable: true,
                    }),
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
        let type_str = params.get::<String>("type").unwrap_or_else(|| "fact".to_string());
        let mem_type = MemoryManager::parse_memory_type(&type_str);

        // Parse tags
        let tags: Vec<String> = params.get::<Vec<String>>("tags").unwrap_or_default();
        let tags: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();

        // Create and save the memory
        let entry = MemoryEntry::new(mem_type.clone(), &content, "agent", &tags);
        match self.memory.add(entry) {
            Ok(()) => Ok(ToolOutput::success(format!(
                "Memory saved: [{}] {}",
                mem_type,
                content.chars().take(100).collect::<String>()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to save memory: {}", e))),
        }
    }
}

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
    fn name(&self) -> &str { "update_memory" }

    fn description(&self) -> &str {
        "Update an existing memory entry when information has changed or been refined."
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
                        description: "Updated content".to_string(),
                        nullable: false,
                    }),
                ])),
                required: vec!["id".to_string(), "content".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let id = match params.get::<String>("id") {
            Some(i) if !i.is_empty() => i,
            _ => return Ok(ToolOutput::error("Memory ID is required")),
        };

        let content = match params.get::<String>("content") {
            Some(c) if !c.trim().is_empty() => c,
            _ => return Ok(ToolOutput::error("Updated content cannot be empty")),
        };

        match self.memory.update(&id, &content) {
            Ok(()) => Ok(ToolOutput::success(format!("Memory '{}' updated", id))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to update memory: {}", e))),
        }
    }
}

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
    fn name(&self) -> &str { "search_memory" }

    fn description(&self) -> &str {
        "Search project memory for relevant information before starting work."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "search_memory".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    ("query".to_string(), FieldSchema {
                        type_name: "string".to_string(),
                        description: "Search query".to_string(),
                        nullable: false,
                    }),
                ])),
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
            output.push_str(&format!("- [{}] (id: {}) {}\n", mem.r#type, mem.id, mem.content));
        }

        Ok(ToolOutput::success(output))
    }
}

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
    fn name(&self) -> &str { "consolidate_memories" }

    fn description(&self) -> &str {
        "Merge multiple related memories into a single, more comprehensive entry."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "consolidate_memories".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    ("memory_ids".to_string(), FieldSchema {
                        type_name: "array".to_string(),
                        description: "IDs of memories to merge".to_string(),
                        nullable: false,
                    }),
                    ("consolidated_content".to_string(), FieldSchema {
                        type_name: "string".to_string(),
                        description: "The merged, comprehensive content".to_string(),
                        nullable: false,
                    }),
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
        for id in &memory_ids {
            if self.memory.delete(id).is_ok() {
                deleted += 1;
            }
        }

        // Create the consolidated memory
        let entry = MemoryEntry::new(MemoryType::Fact, &content, "agent", &["consolidated"]);
        match self.memory.add(entry) {
            Ok(()) => Ok(ToolOutput::success(format!(
                "Consolidated {} memories into a single entry",
                deleted
            ))),
            Err(e) => Ok(ToolOutput::error(format!("Failed to save consolidated memory: {}", e))),
        }
    }
}
