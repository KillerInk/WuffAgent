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

use std::sync::{Arc, Mutex};

use crate::agents::AgentManager;
use crate::memory::MemoryManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolSchema, ToolResult,
};
use crate::types::AppEvent;

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
        "Run an on-demand self-improvement check: reuses the automatic per-task check's \
         analysis (its lessons, metrics, effect check) without the cooldown/evidence \
         gates, and sends any suggestions to the improvements review panel. \
         Params: agent (profile name; required unless scope is 'fleet'), scope ('agent' \
         default = one profile, or 'fleet' = a cross-agent review of the whole fleet: \
         shared failures → skills/new shared agents, skill maintenance), focus \
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
                        "scope".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "'agent' (default) = review one profile, or \
                                          'fleet' = a cross-agent review of the whole fleet \
                                          (shared failure patterns → skills/new shared agents, \
                                          skill maintenance). With 'fleet', omit agent"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "agent".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Name of the agent profile to review (required unless \
                                          scope is 'fleet')"
                                .to_string(),
                            nullable: true,
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
                required: vec![],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        // 2d: scope — 'agent' (default: review one profile) or 'fleet'
        // (cross-agent review of the whole fleet).
        let scope = params
            .get::<String>("scope")
            .map(|s| s.to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "agent".to_string());
        let focus = params
            .get::<String>("focus")
            .filter(|f| !f.is_empty());

        if !self.memory.config().auto_improve {
            return Ok(ToolOutput::error(
                "auto_improve is off (memory config); enable it to run improvement checks",
            ));
        }

        // 2d: FLEET scope (2b(b)) — the roster (name + description of every
        // known profile) is the only profile-specific input; the check
        // records under the pseudo-agent "fleet" (list_improvement_status)
        // and its suggestions land in the review panel like any other batch.
        if scope == "fleet" {
            let roster: Vec<(String, String)> = self
                .agents
                .list_agents()
                .map(|a| a.into_iter().map(|c| (c.name, c.description)).collect())
                .unwrap_or_default();
            let check = match self
                .memory
                .run_fleet_improvement_check(&roster, focus.as_deref())
            {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolOutput::error(format!(
                        "fleet improvement check {e}"
                    )))
                }
            };
            if !check.is_empty() {
                if let Some(tx) = &self.events {
                    let _ = tx.lock().unwrap().send(AppEvent::ImprovementSuggested {
                        agent_name: "fleet".to_string(),
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
                    "{} improvement suggestion(s) for the FLEET generated — they were sent to \
                     the improvements review panel (it should now be visible):\n{summary}",
                    check.len()
                )));
            }
            return Ok(ToolOutput::success(
                "No improvement suggestions for the fleet (either the LLM found no \
                 cross-agent pattern worth improving, or there is not enough evidence in the \
                 window — see list_improvement_status for the loop state)."
                    .to_string(),
            ));
        }
        if scope != "agent" {
            return Ok(ToolOutput::error(format!(
                "unknown scope '{scope}' (expected 'agent' or 'fleet')"
            )));
        }

        // Default: AGENT scope — review one profile (`agent` is required).
        let agent_name = match params.get::<String>("agent") {
            Some(n) if !n.is_empty() => n,
            _ => {
                let available: Vec<String> = self
                    .agents
                    .list_agents()
                    .map(|a| a.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default();
                return Ok(ToolOutput::error(format!(
                    "agent is required when scope is 'agent'. Available profiles: {}",
                    available.join(", ")
                )));
            }
        };

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

        // 4b: the blocking check + record bookkeeping lives in core
        // (`MemoryManager::run_improvement_check`, shared with the UI's
        // "run check now" button). Only a check that ran to term records;
        // timeout/failure surface as explicit errors here.
        let check = match self
            .memory
            .run_improvement_check(&agent_config, focus.as_deref())
        {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "improvement check for '{agent_name}' {e}"
                )))
            }
        };

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

    /// 2d: `scope` is None = agent scope (the default); an empty `agent`
    /// omits the parameter entirely (same as before, for the missing-agent test).
    fn call(
        tool: &RunSelfImprovementTool,
        agent: &str,
        focus: Option<&str>,
        scope: Option<&str>,
    ) -> ToolResult<ToolOutput> {
        let mut values = std::collections::HashMap::new();
        if !agent.is_empty() {
            values.insert("agent".to_string(), serde_json::json!(agent));
        }
        if let Some(f) = focus {
            values.insert("focus".to_string(), serde_json::json!(f));
        }
        if let Some(s) = scope {
            values.insert("scope".to_string(), serde_json::json!(s));
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
        let (ok, msg) = outcome(call(&tool, "", None, None));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("agent is required when scope is 'agent'"), "got: {msg}");
        assert!(msg.contains("coder"), "got: {msg}");
    }

    #[test]
    fn test_unknown_agent_lists_profiles() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let (ok, msg) = outcome(call(&tool, "nope", None, None));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(msg.contains("coder"), "got: {msg}");
    }

    #[test]
    fn test_no_llm_client_reports_no_suggestions() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let (ok, msg) = outcome(call(&tool, "coder", Some("reduce errors"), None));
        assert!(ok, "got: {msg}");
        assert!(msg.contains("No improvement suggestions for 'coder'"), "got: {msg}");
    }

    /// 4b: the shared blocking helper returns `Ok(empty)` when no LLM client
    /// is configured (nothing to improve) AND records the per-agent check —
    /// so the evidence gate re-arms and the no-op streak tracks the empty
    /// result. This is the bookkeeping the tool relied on before 4b, now in
    /// core (shared with the UI's "run check now" button).
    #[test]
    fn test_run_improvement_check_no_llm_records_check() {
        let c = ctx();
        let agent = c.agents.get_agent("coder").expect("coder profile");
        let result = c.memory.run_improvement_check(&agent, None);
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        assert!(result.unwrap().is_empty());
        // The check was recorded (2a): last_check set, runs reset, and the
        // empty result bumped the no-op streak.
        let st = c.memory.agent_improvement_state("coder");
        assert!(st.last_check.is_some(), "check must be recorded per-agent");
        assert_eq!(st.runs_since_check, 0);
        assert_eq!(st.no_op_streak, 1);
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
        let (ok, msg) = outcome(call(&tool, "coder", None, None));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("auto_improve is off"), "got: {msg}");
    }

    #[test]
    fn test_schema_scope_and_agent_optional() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let schema = tool.parameters_schema();
        assert_eq!(schema.name, "run_self_improvement");
        let input = schema.input_type.as_ref().unwrap();
        let props = input.properties.as_ref().unwrap();
        assert!(props.contains_key("scope"));
        assert!(props.contains_key("agent"));
        assert!(props.contains_key("focus"));
        // 2d: `agent` is validated at runtime (required for 'agent' scope),
        // so the schema itself requires nothing up front.
        assert!(input.required.is_empty(), "got: {:?}", input.required);
    }

    /// 2d: `scope: "fleet"` runs the cross-agent review — with no LLM client
    /// it reports "no suggestions for the fleet" AND records the check under
    /// the "fleet" pseudo-agent (no-op streak tracked, like a per-agent run).
    #[test]
    fn test_fleet_scope_no_llm_records_fleet_check() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory.clone(), c.agents, None);
        let (ok, msg) = outcome(call(&tool, "", None, Some("fleet")));
        assert!(ok, "got: {msg}");
        assert!(msg.contains("No improvement suggestions for the fleet"), "got: {msg}");
        let st = c.memory.agent_improvement_state("fleet");
        assert!(st.last_check.is_some(), "fleet check must be recorded");
        assert_eq!(st.runs_since_check, 0);
        assert_eq!(st.no_op_streak, 1);
    }

    /// 2d: an unrecognized scope is an explicit error (not a silent agent run).
    #[test]
    fn test_unknown_scope_is_explicit() {
        let c = ctx();
        let tool = RunSelfImprovementTool::new(c.memory, c.agents, None);
        let (ok, msg) = outcome(call(&tool, "coder", None, Some("galaxy")));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("unknown scope 'galaxy'"), "got: {msg}");
    }
}
