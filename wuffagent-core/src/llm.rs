//! LLM client abstraction for agents.
//!
//! Defines the `LlmClient` trait (minimal interface for LLM interaction)
//! and adapters that wrap `ChatClient` for use by agents.

use async_trait::async_trait;
use std::sync::Arc;

use crate::activity::{ActivityTracker, LabeledLlm};
use crate::client::ChatClient;
use crate::types::{Message, Usage};

/// Lightweight LLM client interface for agents.
/// Simpler than `ChatClientLike` — no session management, just request/response.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a non-streaming request and return the full response.
    async fn complete(&self, messages: &[Message]) -> Result<String, String>;

    /// Send a non-streaming request and return the full response plus the
    /// server-reported token usage (`None` when the server reports none, or
    /// for implementations that don't expose it — the default just calls
    /// [`Self::complete`] with no usage). 1c: the self-improvement loop uses
    /// this to cost its own checks.
    async fn complete_with_usage(
        &self,
        messages: &[Message],
    ) -> Result<(String, Option<Usage>), String> {
        Ok((self.complete(messages).await?, None))
    }

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
#[derive(Clone)]
pub struct ChatClientAdapter {
    client: Arc<ChatClient>,
    /// Optional activity instrumentation (status-bar visibility). The
    /// adapter itself does NOT instrument — call sites opt in via
    /// [`Self::labeled`], so coverage is explicit and nothing is
    /// double-counted.
    activity: Option<Arc<ActivityTracker>>,
}

impl ChatClientAdapter {
    pub fn new(client: ChatClient) -> Self {
        Self {
            client: Arc::new(client),
            activity: None,
        }
    }

    /// Attach the shared [`ActivityTracker`] (bootstrap only; the adapter is
    /// cloned everywhere, so the tracker travels with the clones).
    pub fn with_activity(mut self, tracker: Arc<ActivityTracker>) -> Self {
        self.activity = Some(tracker);
        self
    }

    /// An instrumented view of this adapter for one labeled call
    /// (begin → tg per chunk → finish → drop). `None` when no tracker was
    /// attached via [`Self::with_activity`].
    pub fn labeled(
        &self,
        label: &str,
        session_id: Option<String>,
    ) -> Option<Arc<LabeledLlm>> {
        let tracker = self.activity.as_ref()?;
        let inner: Arc<dyn LlmClient> = Arc::new(self.clone());
        Some(LabeledLlm::new(inner, tracker.clone(), label, session_id))
    }
}

#[async_trait]
impl LlmClient for ChatClientAdapter {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        // Clone the handle to avoid holding any lock across the await
        let client = self.client.clone();
        match client.complete_messages(messages, None).await {
            Ok((response, _)) => Ok(response),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn complete_with_usage(
        &self,
        messages: &[Message],
    ) -> Result<(String, Option<Usage>), String> {
        let client = self.client.clone();
        match client.complete_messages(messages, None).await {
            Ok((response, usage)) => Ok((response, usage)),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let arc = self.client.clone();
        match ChatClient::stream_with_messages_arc(
            &arc,
            messages,
            None,
            move |chunk: String, _is_thinking: bool| {
                chunk_handler(chunk);
                Ok(())
            },
            move |_| {},
            move |_| {}, // prompt progress: not needed for summarization
            None,
        )
        .await
        {
            Ok((msg, _)) => {
                if msg.content.is_empty() {
                    Err("No response from streaming".to_string())
                } else {
                    Ok(msg.content)
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
}


