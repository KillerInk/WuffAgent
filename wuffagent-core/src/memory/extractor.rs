use super::types::MemoryEntry;
use super::manager::MemoryManager;
use crate::agents::llm_client::LlmClient;
use crate::types::Message;

/// Extract memories from conversation history.
/// This is called after an agent task completes or at end of session.
pub async fn extract_memories(
    manager: &MemoryManager,
    messages: &[Message],
    source: &str,
    llm_client: Option<&dyn LlmClient>,
) -> Result<Vec<MemoryEntry>, String> {
    use super::types::MemoryType;

    if !manager.config().enabled || !manager.config().auto_extract_after_task {
        return Ok(Vec::new());
    }

    let llm_client = match llm_client {
        Some(client) => client,
        None => {
            tracing::debug!("No LLM client available for memory extraction");
            return Ok(Vec::new());
        }
    };

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

    // Build extraction prompt (delegates to shared helper)
    let prompt = build_extraction_prompt_from_conversation(&manager.config().project, &conversation);

    // Call LLM to extract memories
    let messages = vec![Message {
        role: "user".to_string(),
        content: prompt,
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];

    let response = match llm_client.complete(&messages).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Memory extraction LLM call failed: {}", e);
            return Ok(Vec::new());
        }
    };

    // Parse JSON response
    let trimmed = response.trim();
    let entries = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<serde_json::Value>>(trimmed)
            .map_err(|e| format!("Failed to parse extraction response: {}", e))?
    } else {
        let start = trimmed.find('[').ok_or("No JSON array in response")?;
        let end = trimmed.rfind(']').ok_or("No JSON array in response")?;
        serde_json::from_str::<Vec<serde_json::Value>>(&trimmed[start..=end])
            .map_err(|e| format!("Failed to parse extraction response: {}", e))?
    };

    let mut result = Vec::new();
    for item in entries {
        let mem_type = match item.get("type").and_then(|t| t.as_str()) {
            Some("fact") => MemoryType::Fact,
            Some("lesson") => MemoryType::Lesson,
            Some("decision") => MemoryType::Decision,
            Some("context") => MemoryType::Context,
            Some("goal") => MemoryType::Goal,
            _ => continue,
        };
        let content = item.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let tags: Vec<String> = item.get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        if !content.is_empty() {
            let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            result.push(MemoryEntry::new(mem_type, &content, source, &tag_refs));
        }
    }

    tracing::info!("Extracted {} memories from conversation", result.len());
    Ok(result)
}

/// Build an extraction prompt from messages (public API).
pub fn build_extraction_prompt(project: &str, messages: &[super::super::types::Message]) -> String {
    let recent: Vec<String> = messages.iter()
        .rev()
        .take(30)
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect();
    build_extraction_prompt_from_conversation(project, &recent.join("\n"))
}

/// Internal helper used by `extract_memories` to build the prompt from a pre-joined conversation string.
pub(crate) fn build_extraction_prompt_from_conversation(project: &str, conversation: &str) -> String {
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

    #[tokio::test]
    async fn test_extract_memories_no_llm() {
        let dir = tempfile::tempdir().unwrap();
        let manager = super::super::manager::MemoryManager::new(
            super::super::types::MemoryConfig {
                memories_dir: Some(dir.path().to_str().unwrap().to_string()),
                ..Default::default()
            }
        ).unwrap();
        let messages = vec![
            Message {
                role: "user".to_string(),
                content: "Fix the shell tool".to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];
        let result = extract_memories(&manager, &messages, "test", None).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
