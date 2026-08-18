use async_trait::async_trait;
use std::sync::Arc;

use crate::client::ChatClient;
use crate::types::Message;

/// Lightweight LLM client interface for agents.
/// Simpler than `ChatClientLike` — no session management, just request/response.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a non-streaming request and return the full response.
    async fn complete(&self, messages: &[Message]) -> Result<String, String>;

    /// Send a streaming request, yielding chunks via the callback and returning the accumulated response.
    async fn stream(
        &self,
        messages: &[Message],
        chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String>;
}

/// Adapter that wraps `ChatClient` to implement `LlmClient`.
/// `ChatClient` is `Clone` (its shared state is behind `Arc`), so we clone a handle
/// before each call to avoid holding a lock across an `.await` boundary.
pub struct ChatClientAdapter {
    client: Arc<ChatClient>,
}

impl ChatClientAdapter {
    pub fn new(client: ChatClient) -> Self {
        Self {
            client: Arc::new(client),
        }
    }
}

#[async_trait]
impl LlmClient for ChatClientAdapter {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let prompt = messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        // Clone the handle to avoid holding any lock across the await
        let client = self.client.clone();
        match client.send_message(&prompt).await {
            Ok((response, _)) => Ok(response),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let prompt = messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        // Clone the handle to avoid holding any lock across the await
        let client = self.client.clone();
        match client
            .stream_message_with_usage(&prompt, move |chunk| {
                chunk_handler(chunk.clone());
                Ok(())
            })
            .await
        {
            Ok(_) => {
                let conv = self.client.conversation();
                let lock = conv.lock().unwrap();
                let last_msg = lock.iter().rev().find(|m| m.role == "assistant" && !m.content.is_empty());
                match last_msg {
                    Some(m) => Ok(m.content.clone()),
                    None => Err("No response from streaming".to_string()),
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Extended LlmClient that supports tool definitions.
pub struct ToolLlmClient {
    client: Arc<ChatClient>,
    tool_defs: Vec<crate::tools::ToolDefinition>,
}

impl ToolLlmClient {
    pub fn new(client: ChatClient, tool_defs: Vec<crate::tools::ToolDefinition>) -> Self {
        Self {
            client: Arc::new(client),
            tool_defs,
        }
    }
}

#[async_trait]
impl LlmClient for ToolLlmClient {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let prompt = messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        let client = self.client.clone();
        let tools = self.tool_defs.clone();
        match client.send_message_with_tools(&prompt, Some(&tools)).await {
            Ok((response, _)) => Ok(response),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let prompt = messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        let client = self.client.clone();
        let tools = self.tool_defs.clone();
        match client
            .stream_message_with_tools_and_usage(&prompt, Some(&tools), move |chunk| {
                chunk_handler(chunk.clone());
                Ok(())
            })
            .await
        {
            Ok(_) => {
                let conv = client.conversation();
                let lock = conv.lock().unwrap();
                let last_msg = lock.iter().rev().find(|m| m.role == "assistant" && !m.content.is_empty());
                match last_msg {
                    Some(m) => Ok(m.content.clone()),
                    None => Err("No response from streaming".to_string()),
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
}
