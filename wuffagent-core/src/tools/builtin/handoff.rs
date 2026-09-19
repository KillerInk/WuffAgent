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
mod tests;
