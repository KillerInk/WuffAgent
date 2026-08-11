use jsonschema::Validator;
use tracing;
use uuid::Uuid;

use super::traits::{Agent, AgentError, AgentRole, ChatClientLike};
use super::types::{AgentId, AgentResult, AgentType, ExecutionPlan, Task, TaskStatus};
use crate::types::Message;

/// JSON schema for validating LLM-generated plan responses.
static PLAN_SCHEMA: &str = r#"
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "required": ["plan_id", "user_request", "tasks"],
  "properties": {
    "plan_id": { "type": "string" },
    "user_request": { "type": "string" },
    "tasks": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["id", "description", "agent_type", "input"],
        "properties": {
          "id": { "type": "string" },
          "description": { "type": "string" },
          "agent_type": { "enum": ["research", "coding", "implementation", "general"] },
          "input": { "type": "object" },
          "depends_on": { "type": ["string", "null"] },
          "max_retries": { "type": "integer" },
          "priority": { "type": "integer" }
        }
      }
    }
  }
}"#;

/// System prompt for the Planner Agent.
static PLANNER_SYSTEM_PROMPT: &str = r#"
You are a planning agent for a multi-agent orchestration system.
Your job is to decompose user requests into executable tasks.

OUTPUT FORMAT: Your ENTIRE response must be a single valid JSON object.
Do NOT include any markdown, code fences, explanations, headings, or text outside the JSON.

Return a JSON object with this exact structure:
{
  "plan_id": "plan-<uuid>",
  "user_request": "<the original request>",
  "tasks": [
    {
      "id": "task-<uuid>",
      "description": "<clear actionable description>",
      "agent_type": "research",
      "input": {"tool": "<tool_name>", "arguments": {}},
      "depends_on": null,
      "max_retries": 3,
      "priority": 0
    }
  ]
}

Rules:
1. Break complex requests into atomic, independent tasks where possible.
2. Set depends_on only when a task strictly needs another's output.
3. Prefer parallel execution (no dependencies) for efficiency.
4. Use agent_type matching the work: research for information gathering,
   coding for file manipulation, implementation for execution, general as fallback.
5. Each task's input should contain the tool name and any required parameters.
6. CRITICAL: Return ONLY the raw JSON object. No markdown, no code fences, no explanation, no headings.
"#;

/// LLM-driven Planner Agent.
pub struct PlannerAgent<C: ChatClientLike> {
    id: AgentId,
    client: C,
    system_prompt: String,
}

impl<C: ChatClientLike> PlannerAgent<C> {
    pub fn new(client: C) -> Self {
        Self {
            id: AgentId::generate(),
            client,
            system_prompt: PLANNER_SYSTEM_PROMPT.to_string(),
        }
    }

    async fn call_llm(&self, messages: &[Message]) -> Result<String, AgentError> {
        self.client
            .send_message(messages)
            .await
            .map_err(|e| AgentError::LlmError(e))
    }

    fn parse_plan_response(&self, response: &str) -> Result<ExecutionPlan, AgentError> {
        // Try to extract JSON from the response (LLM may wrap it in markdown)
        let json_str = extract_json_from_response(response);

        // Validate against schema
        let schema = serde_json::from_str(PLAN_SCHEMA)
            .map_err(|e| AgentError::PlanError(format!("Invalid schema: {}", e)))?;
        let validator = Validator::new(&schema)
            .map_err(|e| AgentError::PlanError(format!("Failed to compile schema: {}", e)))?;

        let plan_value: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| AgentError::PlanError(format!("Invalid JSON: {}", e)))?;

        if let Err(errors) = validator.validate(&plan_value) {
            let error_msgs: Vec<String> = validator.iter_errors(&plan_value).map(|e| e.to_string()).collect();
            tracing::warn!("Plan JSON validation failed: {}", error_msgs.join(", "));
            // Fall back to markdown parsing
            return self.parse_markdown_plan(response);
        }

        let plan: ExecutionPlan = serde_json::from_value(plan_value)
            .map_err(|e| AgentError::PlanError(format!("Deserialization failed: {}", e)))?;
        Ok(plan)
    }

    /// Fallback: parse a markdown-formatted plan into an ExecutionPlan.
    fn parse_markdown_plan(&self, response: &str) -> Result<ExecutionPlan, AgentError> {
        tracing::info!("Falling back to markdown plan parsing");

        let lines: Vec<&str> = response.lines().collect();
        let mut tasks = Vec::new();
        let mut current_task_desc = String::new();
        let mut in_task = false;
        for line in &lines {
            let trimmed = line.trim();

            // Detect task items: numbered lists, bullet points, or "###" sections
            if trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains('.') {
                // Numbered list item like "1. Do something" or "1) Do something"
                if let Some(rest) = trimmed.split_once(|c| c == '.' || c == ')') {
                    let desc = rest.1.trim().to_string();
                    if !desc.is_empty() {
                        if !current_task_desc.is_empty() {
                            tasks.push(Task::new(&current_task_desc, AgentType::General, serde_json::json!({})));
                        }
                        current_task_desc = desc;
                        in_task = true;
                        continue;
                    }
                }
            }

            // Bullet point items
            if trimmed.starts_with('-') || trimmed.starts_with('*') {
                let desc = trimmed[1..].trim().to_string();
                if !desc.is_empty() {
                    if !current_task_desc.is_empty() {
                        tasks.push(Task::new(&current_task_desc, AgentType::General, serde_json::json!({})));
                    }
                    current_task_desc = desc;
                    in_task = true;
                    continue;
                }
            }

            // Sub-items (indented under a task)
            if in_task && (trimmed.starts_with("  ") || trimmed.starts_with("\t")) && !trimmed.is_empty() {
                let sub = trimmed.trim();
                if !sub.is_empty() {
                    current_task_desc.push_str(" ");
                    current_task_desc.push_str(sub);
                }
                continue;
            }

            // Phase headers or other headings signal end of current task
            if trimmed.starts_with('#') || trimmed.is_empty() {
                if in_task && !current_task_desc.is_empty() {
                    tasks.push(Task::new(&current_task_desc, AgentType::General, serde_json::json!({})));
                    current_task_desc = String::new();
                    in_task = false;
                }
                continue;
            }

            // Regular text line — accumulate as task description
            if in_task && !trimmed.is_empty() {
                current_task_desc.push_str(" ");
                current_task_desc.push_str(trimmed);
            }
        }

        // Flush last task
        if !current_task_desc.is_empty() {
            tasks.push(Task::new(&current_task_desc, AgentType::General, serde_json::json!({})));
        }

        // If we found no tasks, use the whole response as a single task
        if tasks.is_empty() {
            let summary = response.lines()
                .take(5)
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<&str>>()
                .join(" ");
            tasks.push(Task::new(&summary, AgentType::General, serde_json::json!({})));
        }

        let plan = ExecutionPlan {
            plan_id: format!("plan-{}", Uuid::new_v4()),
            user_request: response.to_string(),
            tasks,
            created_at: chrono::Utc::now(),
            metadata: serde_json::json!({"source": "markdown_fallback"}),
        };

        Ok(plan)
    }

    async fn generate_plan_with_retry(
        &self,
        request: &str,
        context: Option<&serde_json::Value>,
        max_retries: u32,
    ) -> Result<ExecutionPlan, AgentError> {
        let mut last_error = None;

        for attempt in 0..=max_retries {
            let messages = self.build_plan_messages(request, context);
            let response = match self.call_llm(&messages).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("LLM call failed (attempt {}): {}", attempt + 1, e);
                    if attempt == max_retries {
                        return Err(e);
                    }
                    continue;
                }
            };

            tracing::debug!("Planner LLM response (attempt {}): {}", attempt + 1, &response[..response.len().min(200)]);

            match self.parse_plan_response(&response) {
                Ok(plan) => return Ok(plan),
                Err(e) => {
                    tracing::warn!("Plan validation failed (attempt {}): {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| AgentError::PlanError(
            "Failed to generate valid plan after retries. The LLM may not be producing structured JSON output. Consider using a different model or adjusting the prompt.".to_string()
        )))
    }

    fn build_plan_messages(
        &self,
        request: &str,
        context: Option<&serde_json::Value>,
    ) -> Vec<Message> {
        let mut messages = vec![
            Message {
                role: "system".to_string(),
                content: self.system_prompt.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: format!(
                    "Create an execution plan for this request:\n\n{}\n\n{}",
                    request,
                    context.map(|c| format!("Context:\n{}", c)).unwrap_or_default()
                ),
                timestamp: chrono::Utc::now().to_rfc3339(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        messages
    }
}

#[async_trait::async_trait]
impl<C: ChatClientLike> Agent for PlannerAgent<C> {
    fn id(&self) -> &AgentId { &self.id }
    fn name(&self) -> &str { "Planner" }
    fn role(&self) -> AgentRole { AgentRole::Planner }
    fn instructions(&self) -> &str { PLANNER_SYSTEM_PROMPT }
}

#[async_trait::async_trait]
impl<C: ChatClientLike + Send + Sync + 'static> super::traits::PlannerAgent for PlannerAgent<C> {
    async fn generate_plan(
        &self,
        request: &str,
        context: Option<&serde_json::Value>,
    ) -> Result<ExecutionPlan, AgentError> {
        self.generate_plan_with_retry(request, context, 2).await
    }

    async fn refine_plan(
        &self,
        plan: &ExecutionPlan,
        completed: &[AgentResult],
        failed: &[AgentResult],
        context: &serde_json::Value,
    ) -> Result<ExecutionPlan, AgentError> {
        let completed_summary: Vec<String> = completed
            .iter()
            .map(|r| format!("{}: {}", r.task_id, r.summary))
            .collect();
        let failed_summary: Vec<String> = failed
            .iter()
            .map(|r| format!("{}: {} (status: {})", r.task_id, r.summary, r.status))
            .collect();

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: self.system_prompt.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: format!(
                    "Refine the following plan based on execution feedback.\n\n\
                     Original request: {}\n\
                     Completed tasks: {}\n\
                     Failed tasks: {}\n\
                     Context: {}\n\n\
                     Return a new ExecutionPlan with adjustments.",
                     plan.user_request,
                     completed_summary.join(", "),
                     failed_summary.join(", "),
                     context
                ),
                timestamp: chrono::Utc::now().to_rfc3339(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let response = self.call_llm(&messages).await?;
        self.parse_plan_response(&response)
    }

    async fn is_objective_satisfied(
        &self,
        plan: &ExecutionPlan,
        results: &[AgentResult],
    ) -> Result<bool, AgentError> {
        // Hybrid: mechanical check first
        let all_tasks = plan.tasks.len();
        let completed_count = results
            .iter()
            .filter(|r| r.status == TaskStatus::Completed)
            .count();
        let failed_count = results
            .iter()
            .filter(|r| r.status == TaskStatus::Failed)
            .count();

        tracing::debug!(
            "is_objective_satisfied: all={}, completed={}, failed={}",
            all_tasks,
            completed_count,
            failed_count
        );

        // Mechanical: all tasks completed
        if all_tasks > 0 && completed_count == all_tasks {
            return Ok(true);
        }

        // Mechanical: no tasks at all (trivial plan)
        if all_tasks == 0 {
            return Ok(true);
        }

        // If some failed and no more retries, escalate to LLM
        if failed_count > 0 && completed_count + failed_count >= all_tasks {
            // Use LLM to evaluate whether the partial results satisfy the objective
            let partial_summary: Vec<String> = results
                .iter()
                .map(|r| format!("{}: {} ({})", r.task_id, r.summary, r.status))
                .collect();

            let messages = vec![
                Message {
                    role: "system".to_string(),
                    content: "You are evaluating whether a partial execution satisfies the user's objective. Answer with just 'true' or 'false'.".to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    tool_calls: None,
                    tool_call_id: None,
                },
                Message {
                    role: "user".to_string(),
                    content: format!(
                        "Request: {}\nPartial results: {}\n\nIs the objective satisfied?",
                        plan.user_request,
                        partial_summary.join("; ")
                    ),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ];

            let response = self.call_llm(&messages).await?;
            Ok(response.to_lowercase().contains("true"))
        } else {
            // Not all tasks done yet, not all failed — not satisfied
            Ok(false)
        }
    }
}

/// Extract JSON from a response that may be wrapped in markdown code blocks or prose.
fn extract_json_from_response(response: &str) -> String {
    let s = response.trim();

    // Strip markdown code fences if present
    let s = if s.starts_with("```") {
        let end = s.find("```").unwrap_or(s.len());
        s["```".len()..end].trim()
    } else {
        s
    };

    // Find the outermost balanced JSON object using brace counting
    let mut depth = 0;
    let mut start: Option<usize> = None;
    let mut end = None;

    for (i, ch) in s.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if start.is_some() {
                        end = Some(i + 1);
                        break;
                    }
                }
            }
            '"' => {
                // Skip over string contents (basic escape handling)
                let mut j = i + 1;
                while j < s.len() {
                    let bytes = &s.as_bytes()[j..];
                    if bytes.starts_with(b"\\") {
                        j += 2;
                    } else if bytes.starts_with(b"\"") {
                        j += 1;
                        break;
                    } else {
                        j += 1;
                    }
                }
            }
            _ => {}
        }
    }

    match (start, end) {
        (Some(start), Some(end)) if end > start => s[start..end].to_string(),
        _ => s.to_string(),
    }
}
