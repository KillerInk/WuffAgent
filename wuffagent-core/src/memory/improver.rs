use serde::{Deserialize, Serialize};
use tracing;

use super::manager::MemoryManager;
use crate::agents::config::AgentConfig;
use crate::llm::LlmClient;
use crate::types::Message;

/// A suggested improvement to an agent's configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImprovementSuggestion {
    pub agent_name: String,
    /// New prompt text, or None if no prompt change suggested.
    pub prompt_change: Option<String>,
    /// Explanation for why this improvement is suggested.
    pub rationale: String,
    /// Proposals for new specialized agents.
    pub new_agents: Vec<NewAgentProposal>,
}

/// A proposal to create a new agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAgentProposal {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// Gather relevant lesson memories for an improvement check.
///
/// Lessons live in the global memory store and are NOT tagged with the agent
/// name (memory tools save with source "agent"), so searching by agent name
/// alone almost never matches. Instead, relevance to the current task is the
/// primary signal: search by agent name + task text, and fall back to the
/// most recent lessons when the keyword search finds nothing.
fn collect_lessons(manager: &MemoryManager, agent_name: &str, task: &str) -> Vec<String> {
    let from_search = manager.search(&format!("{} {}", agent_name, task));
    let mut lessons: Vec<String> = from_search
        .iter()
        .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
        .map(|m| m.content.clone())
        .collect();
    if lessons.is_empty() {
        lessons = manager
            .get_recent(10)
            .iter()
            .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
            .map(|m| m.content.clone())
            .collect();
    }
    lessons
}

/// Suggest improvements for an agent based on its memory and recent task result.
///
/// Returns a list of suggestions. Empty list means no improvements needed.
pub async fn suggest_improvements(
    manager: &MemoryManager,
    agent_config: &AgentConfig,
    task: &str,
    result: &str,
    llm_client: &dyn LlmClient,
) -> Result<Vec<ImprovementSuggestion>, String> {
    if !manager.config().auto_improve {
        return Ok(Vec::new());
    }

    // Gather relevant lesson memories for this agent
    let lessons = collect_lessons(manager, &agent_config.name, task);

    if lessons.len() < manager.config().improvement_trigger_lessons {
        tracing::debug!(
            "No relevant lessons for agent '{}', skipping improvement check",
            agent_config.name
        );
        return Ok(Vec::new());
    }

    tracing::info!("[MEMORY] Improvement check for agent '{}': found {} relevant lesson(s), triggering LLM analysis", agent_config.name, lessons.len());

    let memories_text = lessons.join("\n");
    let prompt = agent_config.system_prompt.clone();

    let extraction_prompt = format!(
        "You are reviewing an AI agent's performance to suggest improvements.\n\n\
         Agent name: {}\n\
         Agent description: {}\n\
         Current system prompt:\n{}\n\
         \n\
         Recent task: {}\n\
         Result: {}\n\
         \n\
         Relevant lesson memories:\n{}\n\
         \n\
         Analyze whether the agent's system prompt should be improved.\n\
         Consider:\n\
         - What went well? What went wrong?\n\
         - Are there patterns in the lessons that suggest the prompt needs adjustment?\n\
         - Is there a capability gap that would require a new specialized agent?\n\
         \n\
         Return a JSON array of suggestions (empty [] if nothing to improve):\n\
         [\n\
           {{\n\
             \"agent_name\": \"{}\",\n\
             \"prompt_change\": \"new prompt text or null if no change needed\",\n\
             \"rationale\": \"why this change is needed\",\n\
             \"new_agents\": [\n\
               {{\"name\": \"agent_name\", \"description\": \"...\", \"system_prompt\": \"...\", \"allowed_tools\": [\"tool1\", \"tool2\"]}}\n\
             ]\n\
           }}\n\
         ]\n\
         \n\
         Return [] if no improvements are needed.",
        agent_config.name,
        agent_config.description,
        if prompt.is_empty() {
            format!("You are the '{}' agent. {}", agent_config.name, agent_config.description)
        } else {
            prompt
        },
        task,
        result,
        memories_text,
        agent_config.name,
    );

    let messages = vec![Message {
        role: "user".to_string(),
        content: extraction_prompt,
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }];

    let response = llm_client.complete(&messages).await?;

    // Parse JSON response
    let trimmed = response.trim();
    let suggestions = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<ImprovementSuggestion>>(trimmed)
            .map_err(|e| format!("Failed to parse improvement suggestions: {}", e))?
    } else {
        // Try to find JSON in the response
        let start = trimmed.find('[').unwrap_or(0);
        let end = trimmed.rfind(']').unwrap_or(trimmed.len());
        let json = &trimmed[start..=end];
        serde_json::from_str::<Vec<ImprovementSuggestion>>(json)
            .map_err(|e| format!("Failed to parse improvement suggestions: {}", e))?
    };

    if suggestions.is_empty() {
        tracing::debug!("No improvements suggested for agent '{}'", agent_config.name);
    } else {
        tracing::info!(
            "Generated {} improvement suggestion(s) for agent '{}'",
            suggestions.len(),
            agent_config.name
        );
    }

    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryConfig, MemoryEntry, MemoryType};
    use tempfile::tempdir;

    /// Manager with an isolated temp memories dir (default would hit
    /// ~/.wuffagent/memories and leak real entries into the tests).
    fn fresh_manager() -> (MemoryManager, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        (MemoryManager::new(config).unwrap(), dir)
    }

    #[test]
    fn test_suggestion_serialization() {
        let s = ImprovementSuggestion {
            agent_name: "coder".to_string(),
            prompt_change: Some("You are a coding agent.".to_string()),
            rationale: "Better clarity".to_string(),
            new_agents: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        let parsed: ImprovementSuggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_name, "coder");
        assert_eq!(parsed.prompt_change, Some("You are a coding agent.".to_string()));
    }

    #[test]
    fn test_new_agent_proposal_serialization() {
        let prop = NewAgentProposal {
            name: "researcher".to_string(),
            description: "Searches web".to_string(),
            system_prompt: "You are a researcher.".to_string(),
            allowed_tools: vec!["web_search".to_string()],
        };
        let json = serde_json::to_string(&prop).unwrap();
        let parsed: NewAgentProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "researcher");
        assert_eq!(parsed.allowed_tools, vec!["web_search"]);
    }

    #[test]
    fn test_collect_lessons_prefers_task_relevant_search() {
        let (manager, _dir) = fresh_manager();
        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "Use cargo check before running tests in the rust workspace",
                "agent",
                &[],
            ))
            .unwrap();
        manager
            .add(MemoryEntry::new(
                MemoryType::Fact,
                "An unrelated fact about rust crates and packaging",
                "agent",
                &[],
            ))
            .unwrap();

        let lessons = collect_lessons(&manager, "coder", "run the tests in the rust workspace");
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].contains("cargo check"));
    }

    #[test]
    fn test_collect_lessons_falls_back_to_recent_when_no_match() {
        let (manager, _dir) = fresh_manager();
        manager
            .add(MemoryEntry::new(
                MemoryType::Lesson,
                "The shell tool fails when the working directory does not exist",
                "agent",
                &["shell"],
            ))
            .unwrap();

        // No keyword overlap between the query and the stored lesson, so the
        // recent-lessons fallback must surface it instead of skipping the check.
        let lessons = collect_lessons(&manager, "coder", "completely unrelated topic zzz");
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].contains("shell tool"));
    }

    #[test]
    fn test_collect_lessons_empty_store() {
        let (manager, _dir) = fresh_manager();
        assert!(collect_lessons(&manager, "coder", "some task").is_empty());
    }
}