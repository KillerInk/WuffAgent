//! Memory tools: the five `*_memory` tools agents use to manage the
//! project memory store (see `crate::memory`).
//!
//! One file per tool (the builtin convention — `mcp` got the same
//! treatment): this module used to carry all five, which made each tool's
//! schema/validation/execution harder to review. The shared `snippet`
//! preview helper stays here; every tool wraps the same
//! `Arc<MemoryManager>`.

mod consolidate;
mod delete;
mod save;
mod search;
mod update;

pub use consolidate::ConsolidateMemoriesTool;
pub use delete::DeleteMemoryTool;
pub use save::SaveMemoryTool;
pub use search::SearchMemoryTool;
pub use update::UpdateMemoryTool;

/// Short content preview for tool feedback (keeps responses compact for the LLM).
pub(crate) fn snippet(content: &str) -> String {
    content.chars().take(120).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::memory::{MemoryConfig, MemoryEntry, MemoryManager, MemoryType};
    use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolResult};

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
