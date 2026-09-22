use crate::memory::{MemoryAddResult, MemoryEntry, MemoryManager, MemoryType};
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Short content preview for tool feedback (keeps responses compact for the LLM).
fn snippet(content: &str) -> String {
    content.chars().take(120).collect::<String>()
}

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
         Updating an entry revives it if it was marked superseded."
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
                    ("tags".to_string(), FieldSchema {
                        type_name: "array".to_string(),
                        description: "New tags replacing the old ones (optional; omit to keep current tags)".to_string(),
                        nullable: true,
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

        let tags: Option<Vec<String>> = params.get::<Vec<String>>("tags");
        let tags = tags.filter(|t| !t.is_empty());

        match self.memory.update(&id, &content, tags) {
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
