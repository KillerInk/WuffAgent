//! Agent-profile self-modification tools (T1): `list_agents` and
//! `edit_agent_profile`.
//!
//! Both are SHARED-REGISTRY tools: registered once via
//! [`super::register_agent_tools`] with an [`AgentManager`] bound to the same
//! discovery set the UI agent selector uses (primary `~/.wuffagent/agents/`
//! + the project-level search dirs), and gated per profile through
//! `allowed_tools` like any other tool. Unlike the per-execution `handoff` /
//! `restart` tools they need no per-run state — the discovery dirs are static
//! for the lifetime of the process.
//!
//! Edits are written IN PLACE (F3 pattern): the manager is bound to the
//! directory that actually contains the profile, so the F4 history snapshot
//! lands next to the profile file and `AgentManager` stays the single writer
//! of `agents/*.json` (including the rename/canonicalization bookkeeping).
//! Changes apply from the NEXT message: the running turn's system prompt and
//! tool list are fixed when the turn starts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agents::config::{find_agent_file, list_agent_files};
use crate::agents::manager::AgentManager;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema,
};

/// The two profile tools. Removing these from a profile's CURRENT
/// `allowed_tools` requires `allow_self_removal: true` (no self-removal).
pub const PROFILE_TOOL_NAMES: &[&str] = &["list_agents", "edit_agent_profile"];

/// A profile name becomes a file name (`<name>.json`), so anything that would
/// be a bad file name (or a path) is rejected.
fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '?' | '*' | '"' | '<' | '>' | '|'))
}

/// All scan dirs in priority order (primary first, deduplicated) — the same
/// discovery set the UI agent selector and the `handoff` tool use.
fn ordered_dirs(manager: &AgentManager) -> Vec<PathBuf> {
    let mut all = vec![manager.agents_dir().clone()];
    for dir in manager.search_dirs() {
        if !all.contains(dir) {
            all.push(dir.clone());
        }
    }
    all
}

/// Names of every profile (enabled AND disabled) across the scan dirs,
/// deduplicated by name (first dir wins) — for error messages.
fn available_names(manager: &AgentManager) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in ordered_dirs(manager) {
        for (_, cfg) in list_agent_files(&dir) {
            if seen.insert(cfg.name.clone()) {
                names.push(cfg.name);
            }
        }
    }
    names.sort();
    names
}

fn tool_err(e: crate::agents::AgentError) -> ToolError {
    ToolError::Execution(e.to_string())
}

// ─── list_agents ──────────────────────────────────────────────────────────────

/// Lists every agent profile WuffAgent knows about, including disabled ones
/// (an editor needs to find them to re-enable them) and legacy
/// `WorkerConfig` files (reported in migrated `AgentConfig` form).
pub struct ListAgentsTool {
    manager: Arc<AgentManager>,
}

impl ListAgentsTool {
    pub fn new(manager: Arc<AgentManager>) -> Self {
        Self { manager }
    }
}

impl Tool for ListAgentsTool {
    fn name(&self) -> &str {
        "list_agents"
    }

    fn description(&self) -> &str {
        "List all agent profiles WuffAgent knows about: name, description, enabled \
         flag, backing file path, allowed_tools, handoff/restart settings and a \
         system-prompt preview. Disabled profiles are included. Call this before \
         edit_agent_profile to see which profiles exist and what they currently \
         contain."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_agents".to_string(),
            description: "List all agent profiles".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: Vec::new(),
            }),
        }
    }

    fn execute(&self, _params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let mut profiles = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for dir in ordered_dirs(&self.manager) {
            for (path, cfg) in list_agent_files(&dir) {
                if !seen.insert(cfg.name.clone()) {
                    continue;
                }
                let mut preview: String = cfg.system_prompt.replace('\n', " ");
                if preview.chars().count() > 200 {
                    preview = preview.chars().take(200).collect::<String>() + "…";
                }
                profiles.push(serde_json::json!({
                    "name": cfg.name,
                    "description": cfg.description,
                    "enabled": cfg.enabled,
                    "path": path.display().to_string(),
                    "allowed_tools": cfg.allowed_tools,
                    "handoff_enabled": cfg.handoff_enabled,
                    "handoff_targets": cfg.handoff_targets,
                    "restart_enabled": cfg.restart_enabled,
                    "reasoning_effort": cfg.reasoning_effort,
                    "task_timeout_ms": cfg.task_timeout_ms,
                    "system_prompt_chars": cfg.system_prompt.chars().count(),
                    "system_prompt_preview": preview,
                }));
            }
        }
        Ok(ToolOutput::Success(serde_json::json!({
            "profiles": profiles,
            "note": "Edit a profile with edit_agent_profile (name + the fields to \
                     change; omitted fields are left as-is). Changes apply from the \
                     NEXT message.",
        })))
    }
}

// ─── edit_agent_profile ───────────────────────────────────────────────────────

/// Edits an agent profile by name: any subset of the profile's fields can be
/// changed in one call, omitted fields are left unchanged, and `new_name`
/// renames the profile. Writes go through
/// [`AgentManager::edit_agent`] (pre-edit F4 snapshot + rename bookkeeping)
/// bound to the directory that contains the profile, so the file is updated
/// in place and the change is revertible.
pub struct EditAgentProfileTool {
    manager: Arc<AgentManager>,
}

impl EditAgentProfileTool {
    pub fn new(manager: Arc<AgentManager>) -> Self {
        Self { manager }
    }
}

impl Tool for EditAgentProfileTool {
    fn name(&self) -> &str {
        "edit_agent_profile"
    }

    fn description(&self) -> &str {
        "Edit an agent profile by name. Call list_agents first to see the \
         current values; fields omitted from the call are left unchanged. \
         Supported fields: new_name (rename; the name must stay unique and use \
         letters/digits/'-'/only), description, system_prompt, allowed_tools, \
         enabled, task_timeout_ms, reasoning_effort (off/low/medium/high), \
         handoff_enabled, handoff_targets, restart_enabled, shell (object with \
         shell_enabled/allowed_commands/shell_type/shell_timeout_ms/working_dir). \
         A pre-edit history snapshot is created (revertible). Removing \
         list_agents or edit_agent_profile from allowed_tools requires \
         allow_self_removal=true. The change applies from the NEXT message — \
         the running turn keeps its current prompt and tools."
    }

    fn parameters_schema(&self) -> ToolSchema {
        fn opt(desc: &str) -> FieldSchema {
            FieldSchema {
                type_name: "string".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        }
        fn opt_list(desc: &str) -> FieldSchema {
            FieldSchema {
                type_name: "array".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        }
        fn opt_bool(desc: &str) -> FieldSchema {
            FieldSchema {
                type_name: "boolean".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        }
        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            FieldSchema {
                type_name: "string".to_string(),
                description: "Name of the profile to edit (its `name` field, not the file name)"
                    .to_string(),
                nullable: false,
            },
        );
        props.insert(
            "new_name".to_string(),
            opt("New profile name (rename). Must not collide with an existing profile"),
        );
        props.insert("description".to_string(), opt("New description"));
        props.insert(
            "system_prompt".to_string(),
            opt("New full system prompt (replaces the old one)"),
        );
        props.insert(
            "allowed_tools".to_string(),
            opt_list("New list of allowed tool names (replaces the old list; empty = all tools)"),
        );
        props.insert("enabled".to_string(), opt_bool("Enable or disable the profile"));
        props.insert(
            "task_timeout_ms".to_string(),
            FieldSchema {
                type_name: "number".to_string(),
                description: "Per-task timeout in milliseconds (0 = none)".to_string(),
                nullable: true,
            },
        );
        props.insert(
            "reasoning_effort".to_string(),
            opt("Reasoning effort: off, low, medium or high"),
        );
        props.insert(
            "handoff_enabled".to_string(),
            opt_bool("Whether the agent may hand the session to another agent"),
        );
        props.insert(
            "handoff_targets".to_string(),
            opt_list("Allowed handoff target names (empty = any enabled agent)"),
        );
        props.insert(
            "restart_enabled".to_string(),
            opt_bool("Whether the agent may restart WuffAgent"),
        );
        props.insert(
            "shell".to_string(),
            FieldSchema {
                type_name: "object".to_string(),
                description: "New shell config (partial objects ok: shell_enabled, \
                              allowed_commands, shell_type, shell_timeout_ms, \
                              working_dir)"
                    .to_string(),
                nullable: true,
            },
        );
        props.insert(
            "allow_self_removal".to_string(),
            opt_bool("Set true to confirm removing list_agents/edit_agent_profile \
                      from allowed_tools (no self-removal otherwise)"),
        );
        ToolSchema {
            name: "edit_agent_profile".to_string(),
            description: "Edit an agent profile (fields omitted are left unchanged)"
                .to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let name: String = params
            .get("name")
            .ok_or_else(|| ToolError::InvalidParams("name is required".to_string()))?;
        let name = name.trim().to_string();
        if !valid_profile_name(&name) {
            return Err(ToolError::InvalidParams(format!(
                "Invalid profile name '{}' — use letters, digits, '-' or '_' only \
                 (the name becomes a file name)",
                name
            )));
        }

        let new_name: Option<String> = params.get("new_name");
        let new_name = new_name.map(|s: String| s.trim().to_string());
        if let Some(nn) = &new_name {
            if !valid_profile_name(nn) {
                return Err(ToolError::InvalidParams(format!(
                    "Invalid new_name '{}' — use letters, digits, '-' or '_' only",
                    nn
                )));
            }
        }
        let description: Option<String> = params.get("description");
        let system_prompt: Option<String> = params.get("system_prompt");
        let allowed_tools: Option<Vec<String>> = params.get("allowed_tools");
        let enabled: Option<bool> = params.get("enabled");
        let task_timeout_ms: Option<u64> = params.get("task_timeout_ms");
        let reasoning_effort: Option<crate::types::ReasoningEffort> =
            params.get("reasoning_effort");
        let handoff_enabled: Option<bool> = params.get("handoff_enabled");
        let handoff_targets: Option<Vec<String>> = params.get("handoff_targets");
        let restart_enabled: Option<bool> = params.get("restart_enabled");
        let shell: Option<crate::types::ShellConfig> = params.get("shell");
        let allow_self_removal: bool = params.get("allow_self_removal").unwrap_or(false);

        let mut provided: Vec<&str> = Vec::new();
        if new_name.is_some() {
            provided.push("new_name");
        }
        if description.is_some() {
            provided.push("description");
        }
        if system_prompt.is_some() {
            provided.push("system_prompt");
        }
        if allowed_tools.is_some() {
            provided.push("allowed_tools");
        }
        if enabled.is_some() {
            provided.push("enabled");
        }
        if task_timeout_ms.is_some() {
            provided.push("task_timeout_ms");
        }
        if reasoning_effort.is_some() {
            provided.push("reasoning_effort");
        }
        if handoff_enabled.is_some() {
            provided.push("handoff_enabled");
        }
        if handoff_targets.is_some() {
            provided.push("handoff_targets");
        }
        if restart_enabled.is_some() {
            provided.push("restart_enabled");
        }
        if shell.is_some() {
            provided.push("shell");
        }
        if provided.is_empty() {
            return Err(ToolError::InvalidParams(
                "Nothing to change: provide at least one of new_name, description, \
                 system_prompt, allowed_tools, enabled, task_timeout_ms, \
                 reasoning_effort, handoff_enabled, handoff_targets, restart_enabled, shell"
                    .to_string(),
            ));
        }

        let dirs = ordered_dirs(&self.manager);
        let (dir, file, mut cfg) = find_agent_file(&dirs, &name).ok_or_else(|| {
            let available = available_names(&self.manager);
            ToolError::InvalidParams(format!(
                "No agent profile named '{}' found (available: {})",
                name,
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            ))
        })?;

        // No self-removal: dropping the profile tools from the CURRENT
        // allowed_tools list requires the explicit flag (checked against the
        // pre-edit list, so a profile that never had them is unaffected).
        if let Some(new_tools) = &allowed_tools {
            let removed: Vec<&String> = cfg
                .allowed_tools
                .iter()
                .filter(|t| PROFILE_TOOL_NAMES.contains(&t.as_str()) && !new_tools.contains(t))
                .collect();
            if !removed.is_empty() && !allow_self_removal {
                return Err(ToolError::InvalidParams(format!(
                    "Refusing to remove {} from allowed_tools without \
                     allow_self_removal=true — that would strip this profile of \
                     its profile-editing ability",
                    removed
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }

        if let Some(nn) = &new_name {
            if nn != &name && find_agent_file(&dirs, nn).is_some() {
                return Err(ToolError::InvalidParams(format!(
                    "A profile named '{}' already exists — choose a different \
                     new_name or edit that profile instead",
                    nn
                )));
            }
        }

        let old_name = cfg.name.clone();
        let mut applied: Vec<&str> = Vec::new();
        if let Some(v) = new_name.as_ref() {
            if v != &old_name {
                cfg.name = v.clone();
                applied.push("name");
            }
        }
        if let Some(v) = description {
            cfg.description = v;
            applied.push("description");
        }
        if let Some(v) = system_prompt {
            cfg.system_prompt = v;
            applied.push("system_prompt");
        }
        if let Some(v) = allowed_tools {
            cfg.allowed_tools = v;
            applied.push("allowed_tools");
        }
        if let Some(v) = enabled {
            cfg.enabled = v;
            applied.push("enabled");
        }
        if let Some(v) = task_timeout_ms {
            cfg.task_timeout_ms = v;
            applied.push("task_timeout_ms");
        }
        if let Some(v) = reasoning_effort {
            cfg.reasoning_effort = v;
            applied.push("reasoning_effort");
        }
        if let Some(v) = handoff_enabled {
            cfg.handoff_enabled = v;
            applied.push("handoff_enabled");
        }
        if let Some(v) = handoff_targets {
            cfg.handoff_targets = v;
            applied.push("handoff_targets");
        }
        if let Some(v) = restart_enabled {
            cfg.restart_enabled = v;
            applied.push("restart_enabled");
        }
        if let Some(v) = shell {
            cfg.shell_config = v;
            applied.push("shell_config");
        }

        // Anchor to the directory we found (the field is serialized into the
        // file — the UI approve path does the same).
        cfg.agents_dir = dir.clone();
        cfg.agents_search_dirs = Vec::new();

        // F3: write in place through a manager bound to the containing
        // directory, so the snapshot lands next to the profile. Oddly-named
        // files (e.g. `general.json` holding "generalist") are canonicalized
        // (snapshot + rename) first, so the edit touches one predictable file.
        let mgr = AgentManager::new(dir.clone());
        let _ = mgr.canonicalize_profile_file(&old_name, &file).map_err(tool_err)?;
        mgr.edit_agent(&old_name, &cfg).map_err(tool_err)?;
        let final_path = dir.join(format!("{}.json", cfg.name));

        Ok(ToolOutput::Success(serde_json::json!({
            "status": "updated",
            "agent": cfg.name,
            "path": final_path.display().to_string(),
            "applied": applied,
            "history": "A pre-edit snapshot was written to the profile's history \
                        directory (revertible from the agent editor or the \
                        improvements panel)",
            "note": "The change applies from the NEXT message: this agent's system \
                     prompt and tool list were fixed when the current turn \
                     started. The UI agent selector picks the profile up \
                     automatically.",
        })))
    }
}

#[cfg(test)]
mod tests;
