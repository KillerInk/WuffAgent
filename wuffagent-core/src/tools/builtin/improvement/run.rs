//! `run_self_improvement`: an on-demand self-improvement check for an agent
//! (2a).
//!
//! The per-task check in `AgentEngine::post_task_maintenance` is gated by a
//! cooldown counter AND the new-lesson-evidence gate, and only fires while a
//! task of that agent completes. This tool runs the SAME
//! `MemoryManager::suggest_improvements` path on demand — no cooldown, no
//! evidence gate — for whatever agent the caller names, and emits the
//! suggestions as `AppEvent::ImprovementSuggested` so they land in the
//! review panel like an automatic check's output.

use std::sync::{Arc, LazyLock, Mutex};

use crate::agents::AgentManager;
use crate::memory::MemoryManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolSchema, ToolResult,
};
use crate::types::AppEvent;

/// Cached tokio current-thread runtime for use inside spawn_blocking calls
/// (same pattern as `web_search`: never build a runtime per call).
static BLOCKING_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build blocking runtime")
});

/// Run an async block to completion. Reuses the ambient runtime handle when
/// available (e.g. inside `spawn_blocking`), otherwise the cached
/// `BLOCKING_RUNTIME` (e.g. in tests).
macro_rules! block_on {
    ($expr:expr) => {{
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on($expr),
            Err(_) => BLOCKING_RUNTIME.block_on($expr),
        }
    }};
}

/// Hard ceiling for one on-demand improvement check (the LLM call's own HTTP
/// timeout usually fires first; this is a backstop so a wedged client cannot
/// hold the agent's tool loop forever).
const CHECK_TIMEOUT_SECS: u64 = 180;

/// Tool that triggers one improvement check for a named agent.
pub struct RunSelfImprovementTool {
    memory: Arc<MemoryManager>,
    agents: Arc<AgentManager>,
    /// App event channel (None in tests): suggestions are routed to the egui
    /// review panel through `AppEvent::ImprovementSuggested`.
    events: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
}

impl RunSelfImprovementTool {
    pub fn new(
        memory: Arc<MemoryManager>,
        agents: Arc<AgentManager>,
        events: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    ) -> Self {
        Self {
            memory,
            agents,
            events,
        }
    }
}

impl Tool for RunSelfImprovementTool {
    fn name(&self) -> &str {
        "run_self_improvement"
    }

    fn description(&self) -> &str {
        "Run an on-demand self-improvement check for one agent profile: reuses the \
         automatic per-task check's analysis (its lessons, metrics, effect check) \
         without the cooldown/evidence gates, and sends any suggestions to the \
         improvements review panel. Params: agent (required, profile name), focus \
         (optional, what to concentrate the analysis on)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "run_self_improvement".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(std::collections::HashMap::from([
                    (
                        "agent".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Name of the agent profile to review"
                                .to_string(),
                            nullable: false,
                        },
                    ),
                    (
                        "focus".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Optional: what to concentrate the analysis on (e.g. \
                                          'reduce shell tool errors')"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                ])),
                required: vec!["agent".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let agent_name = match params.get::<String>("agent") {
            Some(n) if !n.is_empty() => n,
            _ => {
                let available: Vec<String> = self
                    .agents
                    .list_agents()
                    .map(|a| a.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default();
                return Ok(ToolOutput::error(format!(
                    "agent is required. Available profiles: {}",
                    available.join(", ")
                )));
            }
        };
        let focus = params
            .get::<String>("focus")
            .filter(|f| !f.is_empty());

        if !self.memory.config().auto_improve {
            return Ok(ToolOutput::error(
                "auto_improve is off (memory config); enable it to run improvement checks",
            ));
        }

        let agent_config = match self.agents.get_agent(&agent_name) {
            Some(c) => c,
            None => {
                let available: Vec<String> = self
                    .agents
                    .list_agents()
                    .map(|a| a.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default();
                return Ok(ToolOutput::error(format!(
                    "agent profile '{agent_name}' not found. Available profiles: {}",
                    available.join(", ")
                )));
            }
        };

        let task = focus.unwrap_or_else(|| {
            format!(
                "(on-demand self-improvement check for '{agent_name}' — no single task in context)"
            )
        });
        let result =
            "(no task result; judge the agent from its lesson memories, run metrics and \
             the effect check below)"
                .to_string();
        let stats = crate::agents::RunStats::default();

        let memory = self.memory.clone();
        let fut = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(CHECK_TIMEOUT_SECS),
                memory.suggest_improvements(&agent_config, &task, &result, &stats),
            )
            .await
        };
        let check = match block_on!(fut) {
            Err(_) => {
                return Ok(ToolOutput::error(format!(
                    "improvement check for '{agent_name}' timed out after {CHECK_TIMEOUT_SECS}s"
                )))
            }
            Ok(Err(e)) => {
                return Ok(ToolOutput::error(format!(
                    "improvement check for '{agent_name}' failed: {e}"
                )))
            }
            Ok(Ok(suggestions)) => suggestions,
        };

        // Same bookkeeping as the per-task path (engine.rs): record the check
        // after the attempt so the evidence gate re-arms on the next new
        // lesson, and list_improvement_status reflects the on-demand check.
        self.memory.record_improvement_check();

        if !check.is_empty() {
            if let Some(tx) = &self.events {
                let _ = tx.lock().unwrap().send(AppEvent::ImprovementSuggested {
                    agent_name: agent_name.clone(),
                    suggestions: check.clone(),
                    session_id: String::new(),
                });
            }
            let summary = check
                .iter()
                .map(|s| {
                    let r: String = s.rationale.chars().take(200).collect();
                    format!("- {r}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(ToolOutput::success(format!(
                "{} improvement suggestion(s) for '{agent_name}' generated — they were sent to \
                 the improvements review panel (it should now be visible):\n{summary}",
                check.len()
            )));
        }

        Ok(ToolOutput::success(format!(
            "No improvement suggestions for '{agent_name}' (either the LLM found nothing to \
             improve, or the agent has fewer than the required relevant lessons — see \
             list_improvement_status for the loop state)."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::AgentConfig;
    use crate::memory::MemoryConfig;
    use crate::tools::types::Tool;

    struct Ctx {
        _mem_dir: tempfile::TempDir,
        _agent_dir: tempfile::TempDir,
        memory: Arc<MemoryManager>,
        agents: Arc<AgentManager>,
    }

    /// Temp memory store (no LLM client) + one "coder" profile.
    fn ctx() -> Ctx {
        let mem_dir = tempfile::tempdir().unwrap();
        let agent_dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(mem_dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        let memory = Arc::new(MemoryManager::new(config).unwrap());
        let agents = Arc::new(AgentManager::new(agent_dir.path().to_path_buf()));
        agents
            .add_agent(&AgentConfig {
                name: "coder".to_string(),
                ..Default::default()
            })
            .unwrap();
        Ctx {
            _mem_dir: mem_dir,
            _agent_dir: agent_dir,
            memory,
            agents,
        }
    }

    fn call(
        tool: &RunSelfImprovementTool,
        agent: &str,
        focus: Option<&str>,
    ) -> ToolResult<ToolOutput> {
        let mut values = std::collections::HashMap::new();
        values.insert("agent".to_string(), serde_json::json!(agent));
        if let Some(f) = focus {
            values.insert("focus".to_string(), serde_json::json!(f));
        }
        tool.execute(ToolParams { values })
    }

    fn outcome(res: ToolResult<ToolOutput>) -> (bool, String) {
        match res {
            Ok(ToolOutput::Success(v)) => (true, v.as_str().unwrap_or("").to_string()),
            Ok(ToolOutput::Error(e)) => (false, e),
            Err(e) => panic!("unexpected ToolError: {e}"),
        }
    }

    #[test]
    fn test_missing_agent_lists_profiles() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let (ok, msg) = outcome(call(&tool, "", None));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("agent is required"), "got: {msg}");
        assert!(msg.contains("coder"), "got: {msg}");
    }

    #[test]
    fn test_unknown_agent_lists_profiles() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let (ok, msg) = outcome(call(&tool, "nope", None));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(msg.contains("coder"), "got: {msg}");
    }

    #[test]
    fn test_no_llm_client_reports_no_suggestions() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let (ok, msg) = outcome(call(&tool, "coder", Some("reduce errors")));
        assert!(ok, "got: {msg}");
        assert!(msg.contains("No improvement suggestions for 'coder'"), "got: {msg}");
    }

    #[test]
    fn test_auto_improve_off_is_explicit() {
        let mem_dir = tempfile::tempdir().unwrap();
        let agent_dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(mem_dir.path().to_str().unwrap().to_string()),
            auto_improve: false,
            ..Default::default()
        };
        let memory = Arc::new(MemoryManager::new(config).unwrap());
        let agents = Arc::new(AgentManager::new(agent_dir.path().to_path_buf()));
        agents
            .add_agent(&AgentConfig {
                name: "coder".to_string(),
                ..Default::default()
            })
            .unwrap();

        let tool = RunSelfImprovementTool::new(memory, agents, None);
        let (ok, msg) = outcome(call(&tool, "coder", None));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("auto_improve is off"), "got: {msg}");
    }

    #[test]
    fn test_schema_requires_only_agent() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let schema = tool.parameters_schema();
        assert_eq!(schema.name, "run_self_improvement");
        let props = schema.input_type.as_ref().unwrap().properties.as_ref().unwrap();
        assert!(props.contains_key("agent"));
        assert!(props.contains_key("focus"));
        assert_eq!(
            schema.input_type.as_ref().unwrap().required,
            vec!["agent".to_string()]
        );
    }
}
