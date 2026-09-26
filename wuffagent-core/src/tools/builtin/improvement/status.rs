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
         Params: agent (optional, profile name) — per-agent state (tasks since its last check, \
         no-op streak/backoff, its evidence gate, last effect verdict)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_improvement_status".to_string(),
            description: self.description().to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some(std::collections::HashMap::from([(
                    "agent".to_string(),
                    crate::tools::types::FieldSchema {
                        type_name: "string".to_string(),
                        description: "Optional: show the per-agent improvement-loop state \
                                      (cooldown counter, no-op streak, evidence, effect verdict) \
                                      for this profile"
                            .to_string(),
                        nullable: true,
                    },
                )])),
                required: vec![],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let status = self.memory.improvement_status();
        let last_check = format_ago(status.last_check);
        let agent = params
            .get::<String>("agent")
            .filter(|a| !a.is_empty());

        // 2a: per-agent detail when a profile is named.
        if let Some(name) = &agent {
            let st = status.agents.get(name);
            let known = st.is_some();
            let state = st
                .cloned()
                .unwrap_or_default();
            let mult =
                crate::agents::improvement::no_op_backoff_multiplier(state.no_op_streak.max(1));
            return Ok(ToolOutput::success(format!(
                "Improvement loop for '{name}': auto_improve={}; last check: {}; \
                 tasks since last check: {} (cooldown base {} task(s), backoff x{} = {}); \
                 no-op streak: {}; new evidence since last check: {}; last effect verdict: {}; \
                 lessons in store: {}{}",
                if status.auto_improve { "on" } else { "off" },
                format_ago(state.last_check),
                state.runs_since_check,
                status.improvement_cooldown_tasks,
                mult,
                status.improvement_cooldown_tasks.saturating_mul(mult as usize),
                state.no_op_streak,
                if self
                    .memory
                    .has_new_agent_improvement_evidence(name)
                {
                    "yes"
                } else {
                    "no"
                },
                state
                    .last_effect_verdict
                    .clone()
                    .unwrap_or_else(|| "none (no applied change recorded yet)".to_string()),
                status.lesson_count,
                if known {
                    String::new()
                } else {
                    " (no per-agent check recorded yet — state shown as default)"
                        .to_string()
                }
            )));
        }

        // Global view (legacy v1 semantics) + a compact per-agent listing.
        let mut out = format!(
            "Improvement loop: auto_improve={}; cooldown=at most 1 check per {} completed task(s); \
             last check (global/legacy): {}; new evidence since last check: {}; lessons in store: {}",
            if status.auto_improve { "on" } else { "off" },
            status.improvement_cooldown_tasks,
            last_check,
            if status.has_new_evidence { "yes" } else { "no" },
            status.lesson_count,
        );
        if !status.agents.is_empty() {
            out.push_str("\nPer-agent state (2a):");
            for (name, st) in &status.agents {
                out.push_str(&format!(
                    "\n  {name}: last check {}; {} task(s) since; no-op streak {}; verdict {}",
                    format_ago(st.last_check),
                    st.runs_since_check,
                    st.no_op_streak,
                    st.last_effect_verdict
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                ));
            }
        }
        Ok(ToolOutput::success(out))
    }
}

/// Render a timestamp as "YYYY-MM-DD HH:MM:SS UTC (~N ago)" or "never".
fn format_ago(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match ts {
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

    /// 2a: run with an `agent` param.
    fn run_with_agent(tool: &ListImprovementStatusTool, agent: &str) -> String {
        let out = tool
            .execute(ToolParams {
                values: std::collections::HashMap::from([(
                    "agent".to_string(),
                    serde_json::json!(agent),
                )]),
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
        assert!(out.contains("last check (global/legacy): never"), "got: {out}");
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

    /// 2a: the optional `agent` param renders the per-agent state (cooldown
    /// counter, backoff, evidence gate, verdict).
    #[test]
    fn test_status_per_agent_detail() {
        let (_dir, manager) = fresh_manager();
        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "A lesson for the named agent",
                "test",
                &["agent:coder"],
            ))
            .unwrap();
        manager.record_agent_task_completed("coder");
        manager.record_agent_task_completed("coder");
        manager.record_agent_improvement_check("coder", false); // streak 1
        manager.record_agent_task_completed("coder");
        manager.record_agent_task_completed("coder");
        manager.record_agent_improvement_check("coder", false); // streak 2 -> x2
        manager.record_agent_task_completed("coder");
        manager.record_effect_verdict("coder", "neutral");

        let tool = ListImprovementStatusTool::new(manager);
        let out = run_with_agent(&tool, "coder");
        assert!(out.contains("Improvement loop for 'coder'"), "got: {out}");
        assert!(out.contains("tasks since last check: 1"), "got: {out}");
        assert!(out.contains("no-op streak: 2"), "got: {out}");
        // Default cooldown base is 5 → x2 = 10.
        assert!(out.contains("backoff x2 = 10"), "got: {out}");
        assert!(out.contains("new evidence since last check: no"), "got: {out}");
        assert!(out.contains("last effect verdict: neutral"), "got: {out}");

        // A profile with no recorded state shows defaults + a hint.
        let out = run_with_agent(&tool, "ghost");
        assert!(out.contains("no per-agent check recorded yet"), "got: {out}");
    }

    /// 2a: the global view lists per-agent states when any exist.
    #[test]
    fn test_status_global_lists_per_agent() {
        let (_dir, manager) = fresh_manager();
        manager.record_agent_improvement_check("coder", true);
        let tool = ListImprovementStatusTool::new(manager);
        let out = run(&tool);
        assert!(out.contains("Per-agent state (2a):"), "got: {out}");
        assert!(out.contains("coder: last check"), "got: {out}");
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
