use super::types::MemoryEntry;
use super::manager::MemoryManager;

/// Extract memories from conversation history.
/// This is called after an agent task completes or at end of session.
pub async fn extract_memories(
    manager: &MemoryManager,
    messages: &[super::super::types::Message],
    source: &str,
) -> Result<Vec<MemoryEntry>, String> {
    use super::types::MemoryType;

    if !manager.config().enabled || !manager.config().auto_extract_after_task {
        return Ok(Vec::new());
    }

    // Take last N messages for context
    let recent: Vec<String> = messages.iter()
        .rev()
        .take(30)
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect();

    if recent.is_empty() {
        return Ok(Vec::new());
    }

    let conversation = recent.join("\n");

    // Build extraction prompt
    let prompt = format!(
        "You are analyzing a conversation to extract persistent memories about the project.\n\n\
         Project: {}\n\n\
         Conversation (last 30 messages):\n{}\n\n\
         Extract memories in this JSON format:\n\
         [\n  {{\"type\": \"fact\", \"content\": \"...\", \"tags\": [\"tag1\", \"tag2\"]}},\n  {{\"type\": \"lesson\", \"content\": \"...\", \"tags\": [\"tag1\"]}},\n  {{\"type\": \"decision\", \"content\": \"...\", \"tags\": [\"tag1\"]}}\n]\n\n\
         Rules:\n\
         - Only extract information that is likely useful in future sessions\n\
         - Facts: project structure, architecture decisions, known issues\n\
         - Lessons: things that went wrong, gotchas, patterns to avoid\n\
         - Decisions: explicit choices made with rationale\n\
         - Context: environment details, constraints, dependencies\n\
         - Goals: ongoing objectives or TODOs\n\
         - Do NOT repeat memories that already exist\n\
         - Keep content concise but complete\n\
         - Return an empty array [] if nothing new to extract",
        manager.config().project,
        conversation
    );

    // Use the LLM client to extract memories
    // Note: This requires access to an LLM client, which we'll pass via a callback
    // For now, return empty and implement LLM integration in Phase 3
    tracing::debug!("Memory extraction requested but LLM client not yet integrated");
    Ok(Vec::new())
}

/// Build an extraction prompt for manual use.
pub fn build_extraction_prompt(project: &str, messages: &[super::super::types::Message]) -> String {
    let recent: Vec<String> = messages.iter()
        .rev()
        .take(30)
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect();

    let conversation = recent.join("\n");

    format!(
        "You are analyzing a conversation to extract persistent memories about the project.\n\n\
         Project: {}\n\n\
         Conversation (last 30 messages):\n{}\n\n\
         Extract memories in this JSON format:\n\
         [\n  {{\"type\": \"fact\", \"content\": \"...\", \"tags\": [\"tag1\", \"tag2\"]}},\n  {{\"type\": \"lesson\", \"content\": \"...\", \"tags\": [\"tag1\"]}},\n  {{\"type\": \"decision\", \"content\": \"...\", \"tags\": [\"tag1\"]}}\n]\n\n\
         Rules:\n\
         - Only extract information that is likely useful in future sessions\n\
         - Facts: project structure, architecture decisions, known issues\n\
         - Lessons: things that went wrong, gotchas, patterns to avoid\n\
         - Decisions: explicit choices made with rationale\n\
         - Context: environment details, constraints, dependencies\n\
         - Goals: ongoing objectives or TODOs\n\
         - Do NOT repeat memories that already exist\n\
         - Keep content concise but complete\n\
         - Return an empty array [] if nothing new to extract",
        project,
        conversation
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::super::types::Message;

    #[test]
    fn test_build_extraction_prompt() {
        let messages = vec![
            Message {
                role: "user".to_string(),
                content: "Fix the shell tool".to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: "assistant".to_string(),
                content: "Updated the shell tool config".to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];

        let prompt = build_extraction_prompt("wuffagent", &messages);
        assert!(prompt.contains("wuffagent"));
        assert!(prompt.contains("Fix the shell tool"));
        assert!(prompt.contains("Updated the shell tool config"));
    }
}
