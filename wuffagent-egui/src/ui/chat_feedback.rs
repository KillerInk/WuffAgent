//! S2: user feedback on assistant answers (👍/👎 in the chat area).
//!
//! Each rating is recorded as a `lesson` memory entry tagged
//! `agent:<profile>` + `user-feedback` through the shared [`MemoryManager`],
//! so the self-improvement pipeline can weigh it as outcome evidence.

use wuffagent_core::memory::{MemoryEntry, MemoryManager, MemoryType};
use wuffagent_core::types::ChatMessage;

/// S2: build the lesson entry recording a user rating.
///
/// `profile_name` is the agent profile that produced the answer,
/// `task_snippet` a short excerpt of the user request it answered,
/// `comment` the optional 👎 comment (empty = none). Pure + unit-testable.
pub fn feedback_lesson(profile_name: &str, good: bool, task_snippet: &str, comment: &str) -> MemoryEntry {
    let rating = if good { "good" } else { "bad" };
    let comment = comment.trim();
    let content = format!(
        "User rated assistant answer {} from agent '{}' on '{}'. Comment: {}",
        rating,
        profile_name,
        task_snippet,
        if comment.is_empty() { "none" } else { comment }
    );
    let agent_tag = format!("agent:{}", profile_name);
    MemoryEntry::new(
        MemoryType::Lesson,
        &content,
        "user-feedback",
        &["user-feedback", &agent_tag],
    )
}

/// S2: persist a rating through the shared store (the dedup gate collapses
/// repeats of an identical rating). `Ok(true)` = saved, `Ok(false)` = skipped
/// (memory disabled), `Err` = store failure.
pub fn remember_feedback(
    memory: &MemoryManager,
    profile_name: &str,
    good: bool,
    task_snippet: &str,
    comment: &str,
) -> Result<bool, String> {
    if !memory.config().enabled {
        return Ok(false);
    }
    memory.add(feedback_lesson(profile_name, good, task_snippet, comment)).map(|_| true)
}

/// S2: a short snippet of the user request the assistant message at
/// `assistant_index` was answering (the nearest preceding user message,
/// truncated). Empty when there is no preceding user message.
pub fn task_snippet_for(messages: &[ChatMessage], assistant_index: usize) -> String {
    let text = messages
        .iter()
        .take(assistant_index)
        .rev()
        .find(|m| m.role == "user" && !m.content.trim().is_empty())
        .map(|m| m.content.trim().to_string())
        .unwrap_or_default();
    if text.chars().count() > 200 {
        let mut t: String = text.chars().take(200).collect();
        t.push('…');
        t
    } else {
        text
    }
}

/// ChatApp extension: record a user rating for an assistant message.
impl crate::ui::state::ChatApp {
    /// S2: record the user's rating of the assistant message at `index` in
    /// the selected session. The profile name comes from the session's
    /// current agent selection — messages don't carry their generating agent,
    /// so a profile switched after the fact is attributed to the new one.
    /// On success (or "skipped: memory disabled") the rating is reflected in
    /// the chat state; on store failure nothing is marked so the user can
    /// retry.
    pub(super) fn save_message_feedback(&mut self, index: usize, good: bool, comment: &str) {
        let Some(sid) = self.selected_session_id.clone() else {
            return;
        };
        let Some((profile, snippet)) = self.session_store.get(&sid).map(|r| {
            (
                r.selected_agent.clone().unwrap_or_else(|| "unknown".to_string()),
                task_snippet_for(&r.chat_state.messages, index),
            )
        }) else {
            return;
        };

        match remember_feedback(&self.memory_manager, &profile, good, &snippet, comment) {
            Ok(_) => {
                if let Some(rt) = self.session_store.get_mut(&sid) {
                    rt.chat_state
                        .message_ratings
                        .insert(index, if good { "good" } else { "bad" }.to_string());
                    rt.chat_state.feedback_comment_for = None;
                    rt.chat_state.feedback_comment.clear();
                }
            }
            Err(e) => {
                tracing::warn!("[FEEDBACK] Failed to save user rating: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests;
