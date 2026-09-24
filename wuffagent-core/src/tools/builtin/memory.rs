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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryConfig, MemoryType};

    /// A fresh manager on a temp dir + one entry in it.
    fn manager_with_entry() -> (tempfile::TempDir, Arc<MemoryManager>, String) {
        let dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = MemoryManager::new(config).unwrap();
        let manager = Arc::new(manager);
        let e = MemoryEntry::new(
            MemoryType::Fact,
            "Original content that a tag-only update must not touch",
            "test",
            &["old"],
        );
        let id = e.id.clone();
        manager.add(e).unwrap();
        (dir, manager, id)
    }

    fn call(tool: &UpdateMemoryTool, json: serde_json::Value) -> ToolResult<ToolOutput> {
        let values = json
            .as_object()
            .expect("params must be an object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        tool.execute(ToolParams { values })
    }

    fn success(res: ToolResult<ToolOutput>) -> String {
        let v = match res {
            Ok(v) => v,
            Err(e) => panic!("expected Ok, got ToolError: {}", e),
        };
        match v {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {}", e),
        }
    }

    #[test]
    fn update_memory_without_content_keeps_existing_text() {
        let (_dir, manager, id) = manager_with_entry();
        let tool = UpdateMemoryTool::new(manager.clone());

        // No `content` key at all → existing content kept, tags replaced.
        let msg = success(call(
            &tool,
            serde_json::json!({ "id": id, "tags": ["new-tag"] }),
        ));
        assert!(msg.starts_with("Memory updated."), "got: {}", msg);

        let persisted = manager.find(&id).expect("entry still exists");
        assert_eq!(
            persisted.content, "Original content that a tag-only update must not touch"
        );
        assert_eq!(persisted.tags, vec!["new-tag"]);
    }

    #[test]
    fn update_memory_with_null_content_keeps_existing_text() {
        let (_dir, manager, id) = manager_with_entry();
        let tool = UpdateMemoryTool::new(manager.clone());

        // Explicit null → treated as "keep existing content".
        let msg = success(call(
            &tool,
            serde_json::json!({ "id": id, "content": null }),
        ));
        assert!(msg.starts_with("Memory updated."), "got: {}", msg);

        let persisted = manager.find(&id).expect("entry still exists");
        assert_eq!(
            persisted.content, "Original content that a tag-only update must not touch"
        );
        assert_eq!(persisted.tags, vec!["old"], "tags unchanged when not passed");
    }

    #[test]
    fn update_memory_with_content_replaces_text() {
        let (_dir, manager, id) = manager_with_entry();
        let tool = UpdateMemoryTool::new(manager.clone());

        let msg = success(call(
            &tool,
            serde_json::json!({ "id": id, "content": "Fresh content replacing the old one entirely" }),
        ));
        assert!(msg.starts_with("Memory updated."), "got: {}", msg);

        let persisted = manager.find(&id).expect("entry still exists");
        assert_eq!(persisted.content, "Fresh content replacing the old one entirely");
    }

    #[test]
    fn update_memory_requires_id() {
        let (_dir, manager, _id) = manager_with_entry();
        let tool = UpdateMemoryTool::new(manager.clone());

        let out = call(&tool, serde_json::json!({ "content": "no id given here today" }));
        match out {
            Ok(ToolOutput::Error(e)) => assert!(e.contains("ID"), "got: {}", e),
            Ok(ToolOutput::Success(v)) => panic!("expected error, got: {}", v),
            Err(e) => panic!("unexpected ToolError: {}", e),
        }
    }
}
