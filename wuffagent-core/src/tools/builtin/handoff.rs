use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agents::config::{load_agent_from_dirs, AgentConfig};
use crate::agents::types::HandoffRequest;
use crate::tools::types::{Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// A tool that hands the session over to another agent profile.
///
/// Per-execution tool: each `Agent` with `handoff_enabled` gets its own
/// instance (its own mailbox, agents dir, and target allowlist), injected in
/// `Agent::new` exactly like the per-agent `shell` tool. It is NOT registered
/// in `register_builtins` because it needs execution-specific state.
///
/// Calling it ends the current agent's turn: the tool writes a
/// [`HandoffRequest`] to the per-execution mailbox, `Agent::run_llm_loop`
/// picks it up before the next LLM round, and `Agent::execute` switches to a
/// fresh agent for the target profile on the same conversation store.
pub struct HandoffTool {
    /// Per-execution mailbox consumed by `Agent::run_llm_loop`.
    mailbox: Arc<Mutex<Option<HandoffRequest>>>,
    /// Directory to resolve target agent profiles from (primary, first).
    agents_dir: PathBuf,
    /// Additional directories to scan after the primary one (first-seen name
    /// wins) — same discovery dirs the UI agent selector uses, so the chat
    /// path can hand off to profiles that live outside the config dir.
    search_dirs: Vec<PathBuf>,
    /// Caller's allowlist of target names (empty = any enabled agent).
    targets: Vec<String>,
    /// Tool description (lists currently available agent names).
    description: String,
}

impl HandoffTool {
    pub fn new(
        mailbox: Arc<Mutex<Option<HandoffRequest>>>,
        agents_dir: PathBuf,
        search_dirs: Vec<PathBuf>,
        targets: Vec<String>,
    ) -> Self {
        let all_dirs = Self::ordered_dirs(&agents_dir, &search_dirs);
        let available = Self::available_agents(&all_dirs)
            .into_iter()
            .filter(|name| targets.is_empty() || targets.iter().any(|t| t == name))
            .collect::<Vec<_>>();
        let description = format!(
            "Hand off the session to another agent so it continues the SAME conversation \
             with its own tools and instructions. Call it when your part of the work is \
             done and a different specialist should take over (e.g. after writing a plan, \
             hand off to a coder to implement it). Your turn ends when you call it. \
             Available agents: {}.",
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        );
        Self {
            mailbox,
            agents_dir,
            search_dirs,
            targets,
            description,
        }
    }

    /// All scan dirs in priority order (primary first, deduplicated).
    fn dirs(&self) -> Vec<PathBuf> {
        Self::ordered_dirs(&self.agents_dir, &self.search_dirs)
    }

    fn ordered_dirs(agents_dir: &PathBuf, search_dirs: &[PathBuf]) -> Vec<PathBuf> {
        let mut all_dirs = vec![agents_dir.clone()];
        for dir in search_dirs {
            if !all_dirs.iter().any(|d| d == dir) {
                all_dirs.push(dir.clone());
            }
        }
        all_dirs
    }

    /// Names of enabled agent profiles across all scan dirs, deduplicated by
    /// name (first dir wins) — for the tool description. When the caller has a
    /// target allowlist, only the allowed names are advertised (the others
    /// would just error on call).
    fn available_agents(dirs: &[PathBuf]) -> Vec<String> {
        let mut names = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if let Ok(cfg) = serde_json::from_str::<AgentConfig>(&content) {
                    if cfg.enabled && seen.insert(cfg.name.clone()) {
                        names.push(cfg.name);
                    }
                }
            }
        }
        names.sort();
        names
    }

    /// Resolve the target profile across all scan dirs, enforcing the
    /// caller's allowlist.
    fn resolve(&self, name: &str) -> Result<AgentConfig, String> {
        let dirs = self.dirs();
        let available = Self::available_agents(&dirs);
        let config = load_agent_from_dirs(&dirs, name).ok_or_else(|| {
            format!(
                "No enabled agent profile named '{}' found (available: {})",
                name,
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            )
        })?;
        if !self.targets.is_empty() && !self.targets.iter().any(|t| t == &config.name) {
            return Err(format!(
                "This agent may only hand off to: {}",
                self.targets.join(", ")
            ));
        }
        Ok(config)
    }
}

impl Tool for HandoffTool {
    fn name(&self) -> &str {
        "handoff"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "handoff".to_string(),
            description: "Hand off the session to another agent".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "agent".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Name of the target agent profile (e.g. 'coder')".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "task".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "What the target agent should do next; include the key context it needs (it also sees the full conversation)".to_string(),
                            nullable: false,
                        },
                    );
                    map
                }),
                required: vec!["agent".to_string(), "task".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let agent: String = params
            .get("agent")
            .ok_or_else(|| ToolError::InvalidParams("agent is required".to_string()))?;
        let agent = agent.trim();
        if agent.is_empty() {
            return Err(ToolError::InvalidParams("agent must not be empty".to_string()));
        }
        let task: String = params
            .get("task")
            .unwrap_or_else(|| "Continue the task at hand.".to_string());
        let task = task.trim().to_string();

        let config = self.resolve(agent).map_err(ToolError::InvalidParams)?;

        {
            let mut guard = self.mailbox.lock().unwrap();
            if guard.is_some() {
                return Err(ToolError::Execution(
                    "A handoff is already pending".to_string(),
                ));
            }
            *guard = Some(HandoffRequest {
                agent: config.name.clone(),
                config,
                task,
            });
        }

        Ok(ToolOutput::Success(serde_json::json!({
            "status": "handoff_queued",
            "to": agent,
            "note": "Your turn ends now; the session continues with the target agent."
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp agents dir with an enabled "coder", an enabled "planner" (which
    /// may only hand off to "coder"), and a disabled "ghost".
    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wuffagent_test_handoff_tool_{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("coder.json"),
            r#"{"name":"coder","system_prompt":"Code things."}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("planner.json"),
            r#"{"name":"planner","system_prompt":"Plan things.","handoff_enabled":true,"handoff_targets":["coder"]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("ghost.json"),
            r#"{"name":"ghost","system_prompt":"Ghosts.","enabled":false}"#,
        )
        .unwrap();
        dir
    }

    fn tool(
        dir: &PathBuf,
        search_dirs: Vec<PathBuf>,
        targets: Vec<String>,
    ) -> (HandoffTool, Arc<Mutex<Option<HandoffRequest>>>) {
        let mailbox = Arc::new(Mutex::new(None));
        let tool = HandoffTool::new(mailbox.clone(), dir.clone(), search_dirs, targets);
        (tool, mailbox)
    }

    fn params(agent: &str, task: &str) -> ToolParams {
        ToolParams {
            values: serde_json::to_value(serde_json::json!({
                "agent": agent,
                "task": task,
            }))
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        }
    }

    #[test]
    fn test_handoff_resolves_target_and_writes_mailbox() {
        let dir = fixture_dir("resolve");
        let (t, mailbox) = tool(&dir, vec![], vec![]);

        let result = t.execute(params("coder", "Implement the plan."));
        assert!(result.is_ok(), "unexpected error: {:?}", result);

        let req = mailbox.lock().unwrap().take().expect("request written");
        assert_eq!(req.agent, "coder");
        assert_eq!(req.config.name, "coder");
        assert_eq!(req.task, "Implement the plan.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handoff_unknown_agent_errors() {
        let dir = fixture_dir("unknown");
        let (t, mailbox) = tool(&dir, vec![], vec![]);

        let err = t.execute(params("nope", "Task")).unwrap_err();
        match err {
            ToolError::InvalidParams(msg) => {
                assert!(msg.contains("nope"), "error should name the agent: {}", msg);
                assert!(msg.contains("coder"), "error should list available agents: {}", msg);
            }
            other => panic!("expected InvalidParams, got {:?}", other),
        }
        assert!(mailbox.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handoff_disabled_target_errors() {
        let dir = fixture_dir("disabled");
        let (t, mailbox) = tool(&dir, vec![], vec![]);

        let err = t.execute(params("ghost", "Task")).unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
        assert!(mailbox.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handoff_target_allowlist_enforced() {
        let dir = fixture_dir("allowlist");
        // planner may only hand off to coder.
        let (t, mailbox) = tool(&dir, vec![], vec!["coder".to_string()]);

        assert!(t.execute(params("coder", "Task")).is_ok());
        assert!(mailbox.lock().unwrap().is_some());

        // Fresh mailbox: hand off to a profile outside the allowlist.
        *mailbox.lock().unwrap() = None;
        let (t2, mailbox2) = tool(&dir, vec![], vec!["coder".to_string()]);
        let err = t2.execute(params("planner", "Task")).unwrap_err();
        match err {
            ToolError::InvalidParams(msg) => {
                assert!(msg.contains("only hand off to"), "{}", msg);
            }
            other => panic!("expected InvalidParams, got {:?}", other),
        }
        assert!(mailbox2.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handoff_rejects_second_pending() {
        let dir = fixture_dir("pending");
        let (t, mailbox) = tool(&dir, vec![], vec![]);

        assert!(t.execute(params("coder", "First")).is_ok());
        let err = t.execute(params("coder", "Second")).unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
        assert!(mailbox.lock().unwrap().is_some(), "original request kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handoff_missing_agent_param() {
        let dir = fixture_dir("missing");
        let (t, _) = tool(&dir, vec![], vec![]);

        let p = ToolParams {
            values: serde_json::to_value(serde_json::json!({ "task": "x" }))
                .unwrap()
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        assert!(matches!(t.execute(p), Err(ToolError::InvalidParams(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Temp dir holding a single enabled profile (distinct prompt so dedup
    /// tests can tell the copies apart).
    fn search_fixture_dir(tag: &str, name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wuffagent_test_handoff_search_{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", name)),
            format!(r#"{{"name":"{name}","system_prompt":"{name} things (search dir)."}}"#),
        )
        .unwrap();
        dir
    }

    #[test]
    fn test_handoff_finds_agent_in_search_dir() {
        // "architect" lives ONLY in the search dir (the primary fixture has
        // coder/planner/ghost) — the old single-dir scan found it nowhere.
        let primary = fixture_dir("search-primary");
        let search = search_fixture_dir("architect", "architect");
        let (t, mailbox) = tool(&primary, vec![search.clone()], vec![]);

        assert!(
            t.description().contains("architect"),
            "description must list the search-dir agent: {}",
            t.description()
        );

        t.execute(params("architect", "Implement the plan.")).unwrap();
        let req = mailbox.lock().unwrap().take().expect("request written");
        assert_eq!(req.agent, "architect");
        // Anchored for chained handoffs: the found dir becomes the primary,
        // the rest stay as search dirs.
        assert_eq!(req.config.agents_dir, search);
        assert_eq!(req.config.agents_search_dirs, vec![primary.clone()]);
        let _ = std::fs::remove_dir_all(&primary);
        let _ = std::fs::remove_dir_all(&search);
    }

    #[test]
    fn test_handoff_dedup_primary_wins() {
        let primary = fixture_dir("dedup-primary"); // has "coder" (primary prompt)
        let search = search_fixture_dir("dedup", "coder"); // has "coder" too (search prompt)
        let (t, mailbox) = tool(&primary, vec![search.clone()], vec![]);

        t.execute(params("coder", "Task")).unwrap();
        let req = mailbox.lock().unwrap().take().unwrap();
        assert_eq!(
            req.config.system_prompt, "Code things.",
            "the primary dir's profile must win dedup"
        );
        assert_eq!(req.config.agents_dir, primary);
        let _ = std::fs::remove_dir_all(&primary);
        let _ = std::fs::remove_dir_all(&search);
    }

    #[test]
    fn test_handoff_description_restricted_to_allowlist() {
        let dir = fixture_dir("restrict");
        let (t, _) = tool(&dir, vec![], vec!["coder".to_string()]);

        assert!(t.description().contains("coder"), "{}", t.description());
        assert!(
            !t.description().contains("planner"),
            "disallowed agents must not be advertised: {}",
            t.description()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
