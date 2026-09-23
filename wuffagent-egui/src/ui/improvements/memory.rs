//! Applying improvements + memory bookkeeping for the review panel
//! (split from `improvements/mod.rs`): profile-file location/writing and the
//! F5/I5 memory entries recording dismissals and approvals.

use std::path::PathBuf;

use wuffagent_core::agents::config::{AgentConfig, AgentManager, WorkerConfig};
use wuffagent_core::memory::{MemoryEntry, MemoryManager, MemoryType};

use super::PendingImprovement;

/// F3: find the directory (in priority order) that ACTUALLY contains a
/// profile named `name`: a `.json` file that parses as `AgentConfig` or
/// legacy `WorkerConfig` with a matching `name` field — the same discovery
/// rules as `AgentManager::load_from_dir`, minus the `enabled` filter, so a
/// disabled profile can still be edited.
///
/// Returns `None` when no known directory holds the profile (it was deleted,
/// or it has no backing file at all — "synthetic").
pub fn resolve_agent_dir(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
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
                if cfg.name == name {
                    return Some(dir.clone());
                }
            } else if let Ok(legacy) = serde_json::from_str::<WorkerConfig>(&content) {
                if legacy.name == name {
                    return Some(dir.clone());
                }
            }
        }
    }
    None
}

/// Apply a single pending improvement (F1: uses the user-edited values when
/// present; F3: writes the profile into the directory it ACTUALLY lives in).
/// Returns `(human-readable result message, prompt_applied)` where
/// `prompt_applied` reports whether a prompt change for the EXISTING agent's
/// profile was successfully written (I5: the condition for recording the
/// effect-check marker — field-only approvals and new-agent-only approvals
/// report `false`).
pub fn apply_improvement_detailed(
    agents_dirs: &[PathBuf],
    agent_manager: &AgentManager,
    imp: &PendingImprovement,
) -> (String, bool) {
    let mut parts: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut prompt_applied = false;

    let apply_prompt = imp
        .apply_prompt
        && imp
            .edited_prompt
            .as_deref()
            .or(imp.prompt_change.as_deref())
            .is_some();
    // I2: the profile fields the user actually approved (None = not proposed
    // or not toggled on).
    let do_tools = imp.apply_allowed_tools.then(|| imp.allowed_tools.clone()).flatten();
    let do_reasoning = imp
        .apply_reasoning_effort
        .then(|| imp.reasoning_effort)
        .flatten();
    let do_shell = imp
        .apply_shell_config
        .then(|| imp.shell_config.clone())
        .flatten();
    let do_handoff = imp
        .apply_handoff_targets
        .then(|| imp.handoff_targets.clone())
        .flatten();
    let do_timeout = imp.apply_task_timeout.then(|| imp.task_timeout_ms).flatten();
    let has_field_change =
        do_tools.is_some() || do_reasoning.is_some() || do_shell.is_some() || do_handoff.is_some() || do_timeout.is_some();

    if apply_prompt || has_field_change {
        // F3: locate the profile's ACTUAL directory (primary first) and edit
        // the file in place there, via a manager bound to that directory —
        // which also puts the F4 history snapshot next to the profile.
        // Writing to the primary dir unconditionally would create a shadow
        // copy when the profile lives in a search dir.
        match resolve_agent_dir(agents_dirs, &imp.agent_name) {
            Some(dir) => {
                let mgr = AgentManager::new(dir.clone());
                match mgr.get_agent(&imp.agent_name) {
                    Some(mut config) => {
                        let mut applied: Vec<String> = Vec::new();
                        if apply_prompt {
                            config.system_prompt = imp
                                .edited_prompt
                                .as_deref()
                                .or(imp.prompt_change.as_deref())
                                .unwrap()
                                .to_string();
                            applied.push("prompt".to_string());
                        }
                        if let Some(tools) = do_tools {
                            config.allowed_tools = tools;
                            applied.push("tools".to_string());
                        }
                        if let Some(re) = do_reasoning {
                            config.reasoning_effort = re;
                            applied.push("reasoning".to_string());
                        }
                        if let Some(sc) = do_shell {
                            config.shell_config = sc;
                            applied.push("shell".to_string());
                        }
                        if let Some(ht) = do_handoff {
                            config.handoff_targets = ht;
                            applied.push("handoff".to_string());
                        }
                        if let Some(ms) = do_timeout {
                            config.task_timeout_ms = ms;
                            applied.push("timeout".to_string());
                        }
                        match mgr.edit_agent(&imp.agent_name, &config) {
                            Ok(()) => {
                                // I5: a prompt was actually written to the
                                // profile -> the effect-check marker applies.
                                prompt_applied = prompt_applied || applied.iter().any(|a| a == "prompt");
                                parts.push(format!(
                                    "updated {} for '{}' in {}",
                                    applied.join(", "),
                                    imp.agent_name,
                                    dir.display()
                                ));
                            }
                            Err(e) => errors.push(format!(
                                "failed to update '{}': {}",
                                imp.agent_name, e
                            )),
                        }
                    }
                    None => errors.push(format!(
                        "agent '{}': profile file found in {} but could not be loaded",
                        imp.agent_name,
                        dir.display()
                    )),
                }
            }
            None => errors.push(format!(
                "profile '{}' not found in any agents directory ({}); the profile may have been deleted or it has no backing file — nothing was written",
                imp.agent_name,
                agents_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    for na in &imp.new_agents {
        let mut proposal = na.proposal.clone();
        // F1: the user's edit wins over the LLM text.
        proposal.system_prompt = na.edited_system_prompt.clone();
        let config = AgentConfig {
            name: proposal.name.clone(),
            description: proposal.description.clone(),
            system_prompt: proposal.system_prompt.clone(),
            allowed_tools: proposal.allowed_tools.clone(),
            ..Default::default()
        };
        match agent_manager.add_agent(&config) {
            Ok(()) => parts.push(format!("created new agent '{}'", proposal.name)),
            Err(e) => errors.push(format!(
                "failed to create agent '{}': {}",
                proposal.name, e
            )),
        }
    }

    let msg = if parts.is_empty() && errors.is_empty() {
        "No changes to apply.".to_string()
    } else {
        let mut msg = parts.join("; ");
        if !errors.is_empty() {
            if !msg.is_empty() {
                msg.push_str("; ");
            }
            msg.push_str(&errors.join("; "));
        }
        msg
    };
    (msg, prompt_applied)
}

/// F5: the lesson entry recording that the user rejected `imp`.
///
/// Tagged `improvement-rejected` + `agent:<name>` and worded to name the
/// agent, so `collect_lessons` (improver.rs) finds it through its
/// agent-name+task keyword search and the improver sees the rejection as
/// negative evidence ("do not re-suggest the same change").
pub fn rejection_lesson(imp: &PendingImprovement) -> MemoryEntry {
    let content = format!(
        "Improvement for '{}' rejected by the user: {} — do not re-suggest the same change.",
        imp.agent_name, imp.rationale
    );
    let agent_tag = format!("agent:{}", imp.agent_name);
    MemoryEntry::new(
        MemoryType::Lesson,
        &content,
        "improvement-review",
        &["improvement-rejected", &agent_tag],
    )
}

/// F5: persist the dismissal of `imp` through the shared memory store.
///
/// Goes through the same save path (`MemoryManager::add`) the memory tools
/// use, so the dedup gate collapses a repeated dismissal of the same
/// suggestion. `Ok(())` on both insert and duplicate; `Err` only on store
/// failure (e.g. the memories file could not be written).
pub fn remember_dismissal(memory: &MemoryManager, imp: &PendingImprovement) -> Result<(), String> {
    memory.add(rejection_lesson(imp)).map(|_| ())
}

/// I5: the marker entry recording that the user APPROVED a prompt change for
/// `imp`'s agent.
///
/// Typed `Fact` (NOT `Lesson`) so `collect_lessons`'s Lesson filter ignores
/// it — it is a reference point for the effect check, not evidence. Tagged
/// `improvement-applied` + `agent:<name>` so `latest_applied_marker`
/// (improver.rs) can find it. The content names the agent and the approval
/// date, plus a short excerpt of the applied prompt: the store's fuzzy dedup
/// gate is token-based, so the date alone would not keep re-approvals of
/// different prompts distinct.
pub fn applied_marker(imp: &PendingImprovement) -> MemoryEntry {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prompt = imp
        .edited_prompt
        .as_deref()
        .or(imp.prompt_change.as_deref())
        .unwrap_or_default();
    let excerpt: String = prompt.chars().take(80).collect();
    let content = format!(
        "Prompt for agent '{}' changed via approved improvement on {} (applied prompt starts: '{}')",
        imp.agent_name, date, excerpt
    );
    let agent_tag = format!("agent:{}", imp.agent_name);
    MemoryEntry::new(
        MemoryType::Fact,
        &content,
        "improvement-review",
        &["improvement-applied", &agent_tag],
    )
}

/// I5: persist the approved prompt change of `imp` through the shared memory
/// store (same save path as F5's `remember_dismissal`). `Ok(())` on both
/// insert and duplicate; `Err` only on store failure.
pub fn remember_applied_prompt(
    memory: &MemoryManager,
    imp: &PendingImprovement,
) -> Result<(), String> {
    memory.add(applied_marker(imp)).map(|_| ())
}
