//! `list_improvement_status`: the auto-improvement loop's own state, readable
//! by an agent (1c).
//!
//! The loop is otherwise passive: after each task, `AgentEngine` decides
//! whether an LLM self-improvement check fires (cooldown + new-evidence
//! gate) and the result lands in the egui review panel. This tool lets an
//! agent answer "when did the last check run, and why is/ isn't one due?" —
//! useful when the agent is editing WuffAgent itself and wants to feed or
//! debug the loop.

use std::sync::Arc;

use crate::memory::MemoryManager;
use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

/// Tool reporting the improvement-loop state (read-only, no LLM call).
pub struct ListImprovementStatusTool {
    memory: Arc<MemoryManager>,
}

impl ListImprovementStatusTool {
    pub fn new(memory: Arc<MemoryManager>) -> Self {
        Self { memory }
    }
}

impl Tool for ListImprovementStatusTool {
    fn name(&self) -> &str {
        "list_improvement_status"
    }

    fn description(&self) -> &str {
        "Show the auto-improvement loop's state: when the last self-improvement check ran, \
         whether new lesson evidence has arrived since, the cooldown/auto_improve settings, \
         and the lesson count. Use it to understand why (no) improvement suggestions appear. \
         No parameters."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_improvement_status".to_string(),
            description: self.description().to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: vec![],
            }),
        }
    }

    fn execute(&self, _params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let status = self.memory.improvement_status();
        let last_check = match status.last_check {
            Some(ts) => {
                let secs = chrono::Utc::now().timestamp().saturating_sub(ts.timestamp());
                let ago = if secs < 3_600 {
                    format!("~{}min ago", secs / 60)
                } else if secs < 86_400 {
                    format!("~{}h ago", secs / 3_600)
                } else {
                    format!("~{}d ago", secs / 86_400)
                };
                format!(
                    "{} ({})",
                    ts.format("%Y-%m-%d %H:%M:%S UTC"),
                    ago
                )
            }
            None => "never".to_string(),
        };
        Ok(ToolOutput::success(format!(
            "Improvement loop: auto_improve={}; cooldown=at most 1 check per {} completed task(s); \
             last check: {}; new evidence since last check: {}; lessons in store: {}",
            if status.auto_improve { "on" } else { "off" },
            status.improvement_cooldown_tasks,
            last_check,
            if status.has_new_evidence { "yes" } else { "no" },
            status.lesson_count,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryConfig, MemoryEntry, MemoryManager, MemoryType};
    use crate::tools::types::ToolParams;

    /// Fresh manager on a temp dir (isolated improvement_state.json).
    fn fresh_manager() -> (tempfile::TempDir, Arc<MemoryManager>) {
        let dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let manager = Arc::new(MemoryManager::new(config).unwrap());
        (dir, manager)
    }

    fn run(tool: &ListImprovementStatusTool) -> String {
        let out = tool
            .execute(ToolParams {
                values: std::collections::HashMap::new(),
            })
            .expect("status tool must not error");
        match out {
            ToolOutput::Success(v) => v.as_str().unwrap_or("").to_string(),
            ToolOutput::Error(e) => panic!("expected success, got error: {e}"),
        }
    }

    #[test]
    fn test_status_before_any_check() {
        let (_dir, manager) = fresh_manager();
        let tool = ListImprovementStatusTool::new(manager);
        let out = run(&tool);
        assert!(out.contains("auto_improve=on"), "got: {out}");
        assert!(out.contains("last check: never"), "got: {out}");
        assert!(out.contains("new evidence since last check: no"), "got: {out}");
        assert!(out.contains("lessons in store: 0"), "got: {out}");
    }

    #[test]
    fn test_status_after_check_and_lesson() {
        let (_dir, manager) = fresh_manager();
        let tool = ListImprovementStatusTool::new(manager.clone());

        manager.record_improvement_check();
        let out = run(&tool);
        assert!(!out.contains("last check: never"), "got: {out}");
        assert!(out.contains("new evidence since last check: no"), "got: {out}");

        // A lesson NEWER than the recorded check re-arms the evidence gate.
        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "A fresh lesson that should count as new evidence",
                "test",
                &["agent:coder"],
            ))
            .unwrap();
        let out = run(&tool);
        assert!(out.contains("new evidence since last check: yes"), "got: {out}");
        assert!(out.contains("lessons in store: 1"), "got: {out}");
    }

    #[test]
    fn test_schema_has_no_required_params() {
        let (_dir, manager) = fresh_manager();
        let tool = ListImprovementStatusTool::new(manager);
        let schema = tool.parameters_schema();
        assert_eq!(schema.name, "list_improvement_status");
        assert!(schema.input_type.as_ref().unwrap().required.is_empty());
    }
}
