use std::sync::mpsc;
use tracing;

pub mod engine;
pub mod http;
pub mod sse;
pub mod session;
pub mod reasoning_state;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::types::{Message, Usage};

// Re-export key types so the public API surface is unchanged
pub use http::{build_request, send_message, build_stream_request, ChatRequest, Response, Choice};
pub use sse::{process_sse_line, stream_message, add_streaming_messages};
pub use session::{
    save_session, load_session,
    enqueue_save_failure, retry_pending_saves, has_save_failure,
    clear_save_failure,
    trim_conversation, clear_history, clear_session_messages,
};

#[derive(Clone)]
pub struct ChatClient {
    base_url: String,
    system_prompt: String,
    conversation: Arc<Mutex<Vec<Message>>>,
    http_client: reqwest::Client,
    api_key: Option<String>,
    session_id: Option<String>,
    session_dir: PathBuf,
    max_messages: usize,
    /// Queue of pending save operations when a save fails.
    save_queue: Arc<Mutex<VecDeque<()>>>,
    /// Whether a save failure notification should be shown in the UI.
    save_failed: Arc<Mutex<bool>>,
    /// Encryption key for session files (32 bytes for ChaCha20Poly1305).
    encryption_key: Option<[u8; 32]>,
    /// Channel to send tool execution events to the UI.
    tool_event_tx: Arc<Mutex<Option<mpsc::Sender<crate::types::AppEvent>>>>,
}

impl ChatClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            system_prompt: String::new(),
            conversation: Arc::new(Mutex::new(Vec::new())),
            http_client: reqwest::Client::new(),
            api_key: None,
            session_id: None,
            session_dir: PathBuf::new(),
            max_messages: 100,
            save_queue: Arc::new(Mutex::new(VecDeque::new())),
            save_failed: Arc::new(Mutex::new(false)),
            encryption_key: None,
            tool_event_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_tool_event_sender(&self, tx: mpsc::Sender<crate::types::AppEvent>) {
        *self.tool_event_tx.lock().unwrap() = Some(tx);
    }

    pub fn set_max_messages(&mut self, max_messages: usize) {
        self.max_messages = max_messages;
    }

    pub fn set_url(&mut self, url: &str) {
        self.base_url = url.to_string();
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn set_api_key(&mut self, key: Option<&str>) {
        self.api_key = key.map(|s| s.to_string());
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_prompt = prompt.to_string();
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn conversation(&self) -> &Arc<Mutex<Vec<Message>>> {
        &self.conversation
    }

    pub fn clear_history(&self) {
        session::clear_history(&self.conversation);
    }

    pub fn clear_session_messages(&mut self) {
        session::clear_session_messages(
            &self.conversation,
            &|| save_session(
                self.session_id.as_deref(),
                &self.session_dir,
                &self.conversation,
                self.encryption_key.as_ref(),
                &self.save_queue,
                &self.save_failed,
            ),
        );
    }

    pub fn trim_conversation(&self, max_messages: usize) {
        session::trim_conversation(&self.conversation, max_messages);
    }

    pub fn set_session(&mut self, session_id: Option<String>, session_dir: PathBuf) {
        self.session_id = session_id;
        self.session_dir = session_dir;
    }

    /// Clear the current session (set session_id to None) and wipe the
    /// in-memory conversation. Call this when a session is deleted so that a
    /// subsequent save cannot resurrect the deleted session file from a
    /// stale in-memory conversation buffer.
    pub fn clear_session(&mut self) {
        self.session_id = None;
        self.conversation.lock().unwrap().clear();
    }

    pub fn set_encryption_key(&mut self, key: Option<[u8; 32]>) {
        self.encryption_key = key;
    }

    pub fn session_dir(&self) -> &PathBuf {
        &self.session_dir
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn load_session(&mut self) -> Option<crate::sessions::Session> {
        session::load_session(
            self.session_id.as_deref(),
            &self.session_dir,
            &self.conversation,
            &mut self.system_prompt,
            self.encryption_key.as_ref(),
        )
    }

    pub fn save_session(&self) -> Result<(), anyhow::Error> {
        save_session(
            self.session_id.as_deref(),
            &self.session_dir,
            &self.conversation,
            self.encryption_key.as_ref(),
            &self.save_queue,
            &self.save_failed,
        )
    }

    /// Enqueue a pending save and set the failure flag for UI notification.
    pub fn enqueue_save_failure(&self, error: &anyhow::Error) {
        session::enqueue_save_failure(&self.save_queue, &self.save_failed, error);
    }

    /// Try to retry any pending saves and clear the queue on success.
    pub fn retry_pending_saves(&self) {
        let _ = session::retry_pending_saves(
            &self.save_queue,
            &self.save_failed,
            &|| save_session(
                self.session_id.as_deref(),
                &self.session_dir,
                &self.conversation,
                self.encryption_key.as_ref(),
                &self.save_queue,
                &self.save_failed,
            ),
        );
    }

    /// Returns true if there is a pending save failure notification to show.
    pub fn has_save_failure(&self) -> bool {
        session::has_save_failure(&self.save_failed)
    }

    /// Clear the save failure flag (call after a successful save or user dismissal).
    pub fn clear_save_failure(&self) {
        session::clear_save_failure(&self.save_failed);
    }

    // ── HTTP methods ──────────────────────────────────────────────────────────

    pub async fn send_message(
        &self,
        prompt: &str,
    ) -> Result<(String, Option<Usage>), Error> {
        self.send_message_with_tools(prompt, None).await
    }

    pub async fn send_message_with_tools(
        &self,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
    ) -> Result<(String, Option<Usage>), Error> {
        let request = build_request(
            &self.system_prompt,
            &self.conversation,
            prompt,
            false,
            tools,
        );
        let (content, usage) = send_message(
            &self.http_client,
            &self.base_url,
            self.api_key.as_deref(),
            &request,
        )
        .await?;

        // Update conversation history
        let mut conv = self.conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
        });
        drop(conv);

        self.trim_conversation(self.max_messages);

        Ok((content, usage))
    }

    pub async fn stream_message_with_usage(
        &self,
        prompt: &str,
        callback: impl FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Option<Usage>, Error> {
        self.stream_message_with_tools_and_usage(prompt, None, callback)
            .await
    }

    pub async fn stream_message_with_tools_and_usage(
        &self,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
        mut callback: impl FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Option<Usage>, Error> {
        let request = build_request(
            &self.system_prompt,
            &self.conversation,
            prompt,
            true,
            tools,
        );
        let builder = build_stream_request(
            &self.http_client,
            &self.base_url,
            self.api_key.as_deref(),
            &request,
        );

        let resp = builder.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Http(format!(
                "Server returned {}: {}",
                status, text
            )));
        }

        // Add user message to history
        add_streaming_messages(&self.conversation, prompt);

        // Box the callback to erase the concrete type and adapt signature
        let mut boxed_cb = Box::new(move |chunk: String, _is_thinking: bool| -> Result<(), Error> {
            (callback)(chunk)
        });
        sse::stream_message(resp, &self.conversation, &mut boxed_cb).await
    }

    /// Arc-based streaming method that clones necessary data before calling
    /// the async streaming, avoiding holding a MutexGuard across .await.
    pub async fn stream_message_with_tools_and_usage_arc(
        client: &Arc<Mutex<Self>>,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
        callback: impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Option<Usage>, Error> {
        // Clone the data we need before calling the async method
        let http_client = client.lock().unwrap().http_client.clone();
        let base_url = client.lock().unwrap().base_url.clone();
        let api_key = client.lock().unwrap().api_key.clone();
        let conversation = client.lock().unwrap().conversation.clone();

        // Build the request with the cloned data
        let mut messages = Vec::new();
        {
            let c = client.lock().unwrap();
            let conv = c.conversation.lock().unwrap();
            for msg in &*conv {
                if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
                    continue;
                }
                messages.push(msg.clone());
            }
        }
        messages.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });

        let request = ChatRequest {
            model: "local".to_string(),
            messages,
            stream: true,
            tools: tools.map(|t| t.to_vec()),
        };
        let body = serde_json::to_string(&request)?;

        let mut builder = http_client
            .post(format!("{}/v1/chat/completions", base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body);
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let resp = builder.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::debug!("stream_message (arc) response status {}: {}", status, text);
            return Err(Error::Http(format!(
                "Server returned {}: {}",
                status, text
            )));
        }

        tracing::debug!("stream_message (arc) streaming started");

        // Add user message to history
        add_streaming_messages(&conversation, prompt);

        // Box the callback to erase the concrete type
        let mut boxed_cb = Box::new(callback);
        sse::stream_message(resp, &conversation, &mut boxed_cb).await
    }

    /// Arc-based streaming that also handles `<think>`-wrapped reasoning content
    /// (DeepSeek-R1, Qwen3.x style) by tracking tag boundaries and emitting
    /// separate thinking/non-thinking chunks.
    pub async fn stream_message_with_reasoning_state_arc(
        client: &Arc<Mutex<Self>>,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
        mut callback: impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Option<Usage>, Error> {
        // Clone the data we need before calling the async method
        let http_client = client.lock().unwrap().http_client.clone();
        let base_url = client.lock().unwrap().base_url.clone();
        let api_key = client.lock().unwrap().api_key.clone();
        let conversation = client.lock().unwrap().conversation.clone();

        // Build the request with the cloned data
        let mut messages = Vec::new();
        {
            let c = client.lock().unwrap();
            let conv = c.conversation.lock().unwrap();
            for msg in &*conv {
                if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
                    continue;
                }
                messages.push(msg.clone());
            }
        }
        messages.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });

        let request = ChatRequest {
            model: "local".to_string(),
            messages,
            stream: true,
            tools: tools.map(|t| t.to_vec()),
        };
        let body = serde_json::to_string(&request)?;

        let mut builder = http_client
            .post(format!("{}/v1/chat/completions", base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body);
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let resp = builder.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::debug!("stream_message_with_reasoning_state_arc response status {}: {}", status, text);
            return Err(Error::Http(format!(
                "Server returned {}: {}",
                status, text
            )));
        }

        tracing::debug!("stream_message_with_reasoning_state_arc streaming started");

        // Add user message to history
        add_streaming_messages(&conversation, prompt);

        sse::stream_message(resp, &conversation, &mut callback).await
    }

    // ── Tool call helpers ─────────────────────────────────────────────────────

    /// Check for malformed tool calls in the conversation and return warnings.
    /// A tool call is considered malformed if its arguments are not valid JSON.
    pub fn check_tool_call_warnings(&self) -> Vec<(String, String)> {
        let conv = self.conversation.lock().unwrap();
        let mut warnings = Vec::new();
        
        for msg in conv.iter() {
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    // Try to parse the arguments as JSON
                    if tc.function.arguments.is_empty() {
                        warnings.push((
                            tc.function.name.clone(),
                            "Empty arguments".to_string(),
                        ));
                    } else if !tc.function.arguments.starts_with('{') {
                        warnings.push((
                            tc.function.name.clone(),
                            "Invalid JSON: arguments don't start with '{'".to_string(),
                        ));
                    } else if serde_json::from_str::<serde_json::Value>(&tc.function.arguments).is_err() {
                        warnings.push((
                            tc.function.name.clone(),
                            format!("Malformed JSON arguments: {}", &tc.function.arguments[..tc.function.arguments.len().min(50)]),
                        ));
                    }
                }
            }
        }
        
        warnings
    }

    /// Check if the last assistant message has pending tool calls.
    pub fn has_pending_tool_calls(&self) -> bool {
        let conv = self.conversation.lock().unwrap();
        match conv.last() {
            Some(msg) if msg.role == "assistant" && msg.tool_calls.is_some() => {
                !msg.tool_calls.as_ref().unwrap().is_empty()
            }
            _ => false,
        }
    }

    /// Get the last assistant message from the conversation.
    pub fn get_last_assistant_message(&self) -> Option<Message> {
        let conv = self.conversation.lock().unwrap();
        conv.iter().rev().find(|m| m.role == "assistant").cloned()
    }

    /// Execute pending tool calls in the conversation and add results.
    /// Returns true if there were tool calls to execute, false otherwise.
    pub async fn execute_pending_tool_calls(
        &self,
        tool_manager: &crate::tools::ToolManager,
    ) -> Result<bool, Error> {
        Self::execute_pending_tool_calls_arc(
            &std::sync::Arc::new(std::sync::Mutex::new(self.clone())),
            tool_manager,
        )
        .await
    }

    /// Internal version that takes an Arc<Mutex<ChatClient>> so the caller
    /// can drop the lock before the async operation begins.
    pub async fn execute_pending_tool_calls_arc(
        client: &std::sync::Arc<std::sync::Mutex<Self>>,
        tool_manager: &crate::tools::ToolManager,
    ) -> Result<bool, Error> {
        // Get tool calls from the last assistant message
        let tool_calls = {
            let client = client.lock().unwrap();
            let conv = client.conversation.lock().unwrap();
            match conv.last() {
                Some(msg) if msg.role == "assistant" && msg.tool_calls.is_some() => {
                    msg.tool_calls.clone()
                }
                _ => None,
            }
        };

        let tool_calls = match tool_calls {
            Some(tc) if !tc.is_empty() => tc,
            _ => return Ok(false),
        };

        // Execute each tool call and add result messages
        for tc in tool_calls {
            tracing::debug!(
                "execute_pending_tool_calls: tool={} call_id={} args={}",
                tc.function.name,
                tc.id,
                tc.function.arguments
            );

            // Send start event
            if let Some(tx) = client.lock().unwrap().tool_event_tx.lock().unwrap().as_ref() {
                let _ = tx.send(crate::types::AppEvent::ToolCallStart {
                    tool_name: tc.function.name.clone(),
                    call_id: tc.id.clone(),
                });
            }

            // Parse arguments
            tracing::debug!(
                "execute_pending_tool_calls: tool={} call_id={} raw_args={}",
                tc.function.name,
                tc.id,
                tc.function.arguments
            );
            tracing::debug!(
                "execute_pending_tool_calls: parsing args for tool={} call_id={} args={:?}",
                tc.function.name,
                tc.id,
                &tc.function.arguments[..tc.function.arguments.len().min(200)]
            );
            // Try parsing as direct args first, then as wrapped in values
            let params = if let Ok(p) = serde_json::from_str::<crate::tools::ToolParams>(&tc.function.arguments) {
                tracing::debug!(
                    "execute_pending_tool_calls: parsed args as ToolParams for tool={} call_id={}",
                    tc.function.name,
                    tc.id
                );
                p
            } else if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                // Model sends direct args like {"expression":"2 + 2"}, wrap them
                tracing::debug!(
                    "execute_pending_tool_calls: parsed args as direct JSON values for tool={} call_id={}",
                    tc.function.name,
                    tc.id
                );
                let mut values = std::collections::HashMap::new();
                if let Some(obj) = args.as_object() {
                    for (k, v) in obj {
                        values.insert(k.clone(), v.clone());
                    }
                }
                crate::tools::ToolParams { values }
            } else {
                tracing::warn!(
                    "execute_pending_tool_calls: failed to parse args for tool={} call_id={} args={:?}",
                    tc.function.name,
                    tc.id,
                    &tc.function.arguments[..tc.function.arguments.len().min(100)]
                );
                if let Some(tx) = client.lock().unwrap().tool_event_tx.lock().unwrap().as_ref() {
                    let _ = tx.send(crate::types::AppEvent::ToolCallError {
                        tool_name: tc.function.name.clone(),
                        call_id: tc.id.clone(),
                        error: "Failed to parse arguments".to_string(),
                    });
                }
                continue;
            };

            // Execute the tool
            let result = tool_manager.execute(&tc.function.name, params).await;
            tracing::debug!(
                "execute_pending_tool_calls: tool={} call_id={} result={:?}",
                tc.function.name,
                tc.id,
                &result
            );
            
            // Send complete or error event
            match &result {
                Ok(output) => {
                    if let Some(tx) = client.lock().unwrap().tool_event_tx.lock().unwrap().as_ref() {
                        let result_str = match output {
                            crate::tools::types::ToolOutput::Success(v) => v.to_string(),
                            crate::tools::types::ToolOutput::Error(e) => e.clone(),
                        };
                        let _ = tx.send(crate::types::AppEvent::ToolCallComplete {
                            tool_name: tc.function.name.clone(),
                            call_id: tc.id.clone(),
                            result: result_str,
                        });
                    }
                }
                Err(e) => {
                    if let Some(tx) = client.lock().unwrap().tool_event_tx.lock().unwrap().as_ref() {
                        let _ = tx.send(crate::types::AppEvent::ToolCallError {
                            tool_name: tc.function.name.clone(),
                            call_id: tc.id.clone(),
                            error: e.to_string(),
                        });
                    }
                }
            }

            // Add result message to conversation in formatted format for UI display
            {
                let result_str = result.as_ref().map_or_else(
                    |e| e.to_string(),
                    |r| match r {
                        crate::tools::types::ToolOutput::Success(v) => v.to_string(),
                        crate::tools::types::ToolOutput::Error(e) => e.clone(),
                    },
                );
                let header = crate::types::tool_call_header(&tc.function.name, &result_str);
                let content = format!("{}||{}||{}", header, tc.id, result_str);

                let client = client.lock().unwrap();
                let mut conv = client.conversation.lock().unwrap();
                if let Some(last) = conv.last_mut() {
                    if last.role == "assistant" {
                        // Remove the tool call from the last assistant message
                        if let Some(tcs) = &mut last.tool_calls {
                            tcs.retain(|t| t.id != tc.id);
                        }
                    }
                }
                conv.push(Message {
                    role: "tool".to_string(),
                    content,
                    timestamp: crate::types::format_timestamp(),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
        }

        Ok(true)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::Http(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_no_system_prompt() {
        let client = ChatClient::new("http://localhost:8080");
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            None,
        );
        assert_eq!(request.model, "local");
        assert!(!request.stream);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hello");
    }

    #[test]
    fn test_build_request_with_system_prompt() {
        let mut client = ChatClient::new("http://localhost:8080");
        client.set_system_prompt("You are helpful.");
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            None,
        );
        assert_eq!(request.model, "local");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[0].content, "You are helpful.");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "Hello");
    }

    #[test]
    fn test_build_request_streaming() {
        let client = ChatClient::new("http://localhost:8080");
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            true,
            None,
        );
        assert!(request.stream);
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_build_request_with_tools() {
        let client = ChatClient::new("http://localhost:8080");
        let tools = vec![
            crate::tools::ToolDefinition {
                type_name: "function".to_string(),
                function: crate::tools::ToolFunctionSpec {
                    name: "test_tool".to_string(),
                    description: "A test tool".to_string(),
                    parameters: crate::tools::JsonSchema {
                        type_name: "object".to_string(),
                        properties: None,
                        required: vec![],
                    },
                },
            }
        ];
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Hello",
            false,
            Some(&tools),
        );
        assert!(request.tools.is_some());
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_build_request_includes_history() {
        let client = ChatClient::new("http://localhost:8080");
        {
            let mut conv = client.conversation.lock().unwrap();
            conv.push(Message { role: "user".into(), content: "Hi there".into(), timestamp: String::new(), tool_calls: None, tool_call_id: None });
                        conv.push(Message { role: "assistant".into(), content: "Hello! How can I help?".into(), timestamp: String::new(), tool_calls: None, tool_call_id: None });
        }
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "What's the weather?",
            false,
            None,
        );
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hi there");
        assert_eq!(request.messages[1].role, "assistant");
        assert_eq!(request.messages[1].content, "Hello! How can I help?");
        assert_eq!(request.messages[2].role, "user");
        assert_eq!(request.messages[2].content, "What's the weather?");
    }

    #[test]
    fn test_build_request_skips_empty_assistant_message() {
        let client = ChatClient::new("http://localhost:8080");
        {
            let mut conv = client.conversation.lock().unwrap();
            conv.push(Message { role: "user".into(), content: "Hi".into(), timestamp: String::new(), tool_calls: None, tool_call_id: None });
                        conv.push(Message { role: "assistant".into(), content: String::new(), timestamp: String::new(), tool_calls: None, tool_call_id: None });
        }
        let request = build_request(
            &client.system_prompt,
            &client.conversation,
            "Follow up",
            false,
            None,
        );
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hi");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "Follow up");
    }

    #[test]
    fn test_chat_client_api_key() {
        let mut client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.api_key(), None);
        client.set_api_key(Some("sk-test"));
        assert_eq!(client.api_key(), Some("sk-test"));
    }

    #[test]
    fn test_chat_client_base_url() {
        let client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[tokio::test]
    async fn test_process_sse_line_empty() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_sse_line_done() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("data: [DONE]", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_non_data() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line("id: 1", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_valid_chunk() {
        let client = ChatClient::new("http://localhost:8080");
        // Pre-seed an empty assistant message (caller does this in stream_message)
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });
        drop(conv);

        let sse_data = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line(sse_data, &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert_eq!(captured, vec!["Hello"]);
        let c = client.conversation().lock().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].role, "assistant");
        assert_eq!(c[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_process_sse_line_multiple_chunks() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });
        drop(conv);

        let sse1 = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        process_sse_line(sse1, &mut cb, &client.conversation()).await.unwrap();

        let sse2 = r#"data: {"choices":[{"delta":{"content":" world"}}]}"#;
        let mut captured2 = Vec::new();
        let mut cb2 = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured2.push(s);
            Ok(())
        };
        process_sse_line(sse2, &mut cb2, &client.conversation()).await.unwrap();

        assert_eq!(captured, vec!["Hello"]);
        assert_eq!(captured2, vec![" world"]);
        let c = client.conversation().lock().unwrap();
        assert_eq!(c[0].content, "Hello world");
    }

    #[tokio::test]
    async fn test_process_sse_line_empty_content() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });
        drop(conv);

        let sse_data = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String, _is_thinking: bool| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = process_sse_line(sse_data, &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[test]
    fn test_check_tool_call_warnings_empty_args() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: "".to_string(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "tc1".to_string(),
                call_type: "function".to_string(),
                function: crate::types::ToolFunction {
                    name: "test_tool".to_string(),
                    arguments: "".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, "test_tool");
        assert_eq!(warnings[0].1, "Empty arguments");
    }

    #[test]
    fn test_check_tool_call_warnings_invalid_json() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: "".to_string(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "tc1".to_string(),
                call_type: "function".to_string(),
                function: crate::types::ToolFunction {
                    name: "test_tool".to_string(),
                    arguments: "not json".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, "test_tool");
        assert!(warnings[0].1.contains("Invalid JSON"));
    }

    #[test]
    fn test_check_tool_call_warnings_valid_json() {
        let client = ChatClient::new("http://localhost:8080");
        let mut conv = client.conversation().lock().unwrap();
        conv.push(Message {
            role: "assistant".to_string(),
            content: "".to_string(),
            timestamp: String::new(),
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "tc1".to_string(),
                call_type: "function".to_string(),
                function: crate::types::ToolFunction {
                    name: "test_tool".to_string(),
                    arguments: "{\"key\": \"value\"}".to_string(),
                },
            }]),
            tool_call_id: None,
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert!(warnings.is_empty());
    }
}
