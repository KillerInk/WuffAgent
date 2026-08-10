use futures::StreamExt;
use std::sync::mpsc;
use tracing;

pub mod engine;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::types::{Message, Usage};

#[derive(Serialize, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<crate::tools::ToolDefinition>>,
}

#[derive(Deserialize, Debug)]
pub struct Response {
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Deserialize, Debug)]
pub struct Choice {
    pub message: Message,
}

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
    tool_event_tx: Arc<Mutex<Option<mpsc::Sender<crate::ui::window::AppEvent>>>>,
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

    pub fn set_tool_event_sender(&self, tx: mpsc::Sender<crate::ui::window::AppEvent>) {
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
        self.conversation.lock().unwrap().clear();
    }

    pub fn clear_session_messages(&mut self) {
        self.conversation.lock().unwrap().clear();
        if let Err(e) = self.save_session() {
            eprintln!("Failed to save session after clear: {}", e);
        }
    }

    pub fn trim_conversation(&self, max_messages: usize) {
        let mut conv = self.conversation.lock().unwrap();
        if conv.len() <= max_messages {
            return;
        }
        let system_idx = conv.iter().position(|m| m.role == "system");
        let keep_from = if let Some(idx) = system_idx {
            idx + 1
        } else {
            0
        };
        let trim_at = conv.len().saturating_sub(max_messages);
        let start = keep_from.min(trim_at);
        conv.drain(..start);
    }

    pub fn set_session(&mut self, session_id: Option<String>, session_dir: PathBuf) {
        self.session_id = session_id;
        self.session_dir = session_dir;
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
        let dir = self.session_dir.clone();
        let id = self.session_id.as_ref()?;
        let session = if let Some(key) = &self.encryption_key {
            crate::sessions::decrypt_and_load_session(&dir, id, key)
        } else {
            crate::sessions::load_session(&dir, id)
        };
        let session = session?;
        let mut conv = self.conversation.lock().unwrap();
        *conv = session.messages.clone();
        if !session.system_prompt.is_empty() {
            self.system_prompt = session.system_prompt.clone();
        }
        Some(session)
    }

    pub fn save_session(&self) -> Result<(), anyhow::Error> {
        let id = self.session_id.as_ref().ok_or_else(|| anyhow::anyhow!("no session id"))?;
        let conv = self.conversation.lock().unwrap();
        // Try loading the session; if it's encrypted, fall back to creating a new one
        // (the key will be used to re-encrypt on the next save).
        let mut session = if let Some(key) = &self.encryption_key {
            crate::sessions::decrypt_and_load_session(&self.session_dir, id, key)
                .or_else(|| crate::sessions::load_session(&self.session_dir, id))
                .ok_or_else(|| anyhow::anyhow!("session not found"))?
        } else {
            crate::sessions::load_session(&self.session_dir, id)
                .ok_or_else(|| anyhow::anyhow!("session not found"))?
        };
        session.messages = conv.clone();
        // Retry with exponential backoff for transient failures
        let mut retries = 0;
        loop {
            let save_result = if let Some(key) = &self.encryption_key {
                crate::sessions::save_session_encrypted(&self.session_dir, &session, key)
            } else {
                crate::sessions::save_session_atomic(&self.session_dir, &session)
            };
            match save_result {
                Ok(()) => {
                    // Success: clear any pending queue and failure flag
                    self.clear_save_queue();
                    return Ok(());
                }
                Err(e) if retries < 3 => {
                    retries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50u64.pow(retries as u32)));
                    eprintln!("Session save attempt {} failed: {}, retrying...", retries, e);
                }
                Err(e) => {
                    // All retries exhausted — enqueue for later retry and signal UI
                    self.enqueue_save_failure(&e);
                    return Err(e);
                }
            }
        }
    }

    /// Enqueue a pending save and set the failure flag for UI notification.
    pub fn enqueue_save_failure(&self, error: &anyhow::Error) {
        let mut queue = self.save_queue.lock().unwrap();
        queue.push_back(());
        drop(queue);
        let mut flagged = self.save_failed.lock().unwrap();
        *flagged = true;
        eprintln!("Session save failed, enqueued for retry: {}", error);
    }

    /// Try to retry any pending saves and clear the queue on success.
    pub fn retry_pending_saves(&self) {
        let mut queue = self.save_queue.lock().unwrap();
        let count = queue.len();
        if count == 0 {
            return;
        }
        // Drain the queue and attempt saves
        queue.clear();
        drop(queue);

        // Attempt a single save; if it succeeds, clear the failure flag
        if let Err(e) = self.save_session() {
            // Still failing — re-enqueue and keep the flag
            self.enqueue_save_failure(&e);
        } else {
            let mut flagged = self.save_failed.lock().unwrap();
            *flagged = false;
        }
    }

    /// Returns true if there is a pending save failure notification to show.
    pub fn has_save_failure(&self) -> bool {
        *self.save_failed.lock().unwrap()
    }

    /// Clear the save failure flag (call after a successful save or user dismissal).
    pub fn clear_save_failure(&self) {
        let mut flagged = self.save_failed.lock().unwrap();
        *flagged = false;
    }

    /// Clear the internal retry queue without attempting a save.
    fn clear_save_queue(&self) {
        let mut queue = self.save_queue.lock().unwrap();
        queue.clear();
        drop(queue);
        let mut flagged = self.save_failed.lock().unwrap();
        *flagged = false;
    }

    fn build_request(&self, prompt: &str, stream: bool, tools: Option<&[crate::tools::ToolDefinition]>) -> ChatRequest {
        let mut messages = Vec::new();

        if !self.system_prompt.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: self.system_prompt.clone(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Include conversation history, excluding any in-progress empty assistant message
        let conv = self.conversation.lock().unwrap();
        for msg in &*conv {
            // Skip empty assistant messages that are being accumulated during streaming
            if msg.role == "assistant" && msg.content.is_empty() && msg.tool_calls.is_none() {
                continue;
            }
            messages.push(msg.clone());
        }
        drop(conv);

        messages.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });

        ChatRequest {
            model: "local".to_string(),
            messages,
            stream,
            tools: tools.map(|t| t.to_vec()),
        }
    }

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
        let request = self.build_request(prompt, false, tools);
        let body = serde_json::to_string(&request)?;
        tracing::debug!(
            "send_message (non-stream) request body:\n{}",
            body
        );

        let mut builder = self
            .http_client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .body(body);
        if let Some(ref key) = self.api_key {
            builder = builder.header(
                "Authorization",
                format!("Bearer {}", key),
            );
        }

        let resp = builder.send().await?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !status.is_success() {
            return Err(Error::Http(format!(
                "Server returned {}: {}",
                status, text
            )));
        }

        tracing::debug!("send_message (non-stream) response body:\n{}", text);

        let response: Response = serde_json::from_str(&text)?;

        if response.choices.is_empty() {
            return Err(Error::Stream("Empty response".to_string()));
        }

        let content = response.choices[0].message.content.clone();
        let usage = response.usage.clone();
        tracing::debug!(
            "send_message (non-stream) assistant content (len={}): {:?}",
            content.len(),
            content.chars().take(200).collect::<String>()
        );

        // Update conversation history
        let mut conv = self.conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        });
        drop(conv);

        self.trim_conversation(self.max_messages);

        Ok((content, usage))
    }

    async fn process_sse_line(
        line: &str,
        callback: &mut impl FnMut(String) -> Result<(), Error>,
        conversation: &Arc<Mutex<Vec<Message>>>,
    ) -> Result<Option<Usage>, Error> {
        if line.is_empty() || line == "data: [DONE]" {
            return Ok(None);
        }

        if !line.starts_with("data: ") {
            return Ok(None);
        }

        let data = &line["data: ".len()..];
        // Log the raw SSE data for debugging tool call issues
        if data.contains("tool_calls") || data.contains("function") {
            tracing::debug!("process_sse_line raw SSE data: {}", &data[..data.len().min(500)]);
        }
        let chunk: serde_json::Value = serde_json::from_str(data)?;

        if let Some(text) = chunk
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
        {
            if !text.is_empty() {
                callback(text.to_string())?;

                // Update conversation history
                let mut conv = conversation.lock().unwrap();
                if let Some(last) = conv.last_mut() {
                    last.content.push_str(text);
                }
            }
        }

        // Handle tool_calls in streaming delta chunks
        if let Some(tool_calls) = chunk
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("tool_calls"))
        {
            if let Some(tc_array) = tool_calls.as_array() {
                for tc_chunk in tc_array {
                    // Extract id (may be null/missing in delta chunks after the first)
                    let id = tc_chunk
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    // Extract index (used when id is not present)
                    let index = tc_chunk.get("index").and_then(|v| v.as_u64());
                    let func = tc_chunk.get("function");

                    if let Some(func) = func {
                        let name = func
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = func
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        tracing::debug!(
                            "process_sse_line: tool_call id=? name={} args_len={} args_preview={:?}",
                            name, args.len(),
                            &args[..args.len().min(80)]
                        );

                        // Accumulate partial args from streaming
                        let mut conv = conversation.lock().unwrap();
                        if let Some(last) = conv.last_mut() {
                            tracing::debug!(
                                "process_sse_line: last message role={}, has_tool_calls={}, tool_calls_count={}",
                                last.role,
                                last.tool_calls.is_some(),
                                last.tool_calls.as_ref().map(|t| t.len()).unwrap_or(0)
                            );
                            if last.tool_calls.is_none() {
                                last.tool_calls = Some(Vec::new());
                            }
                            let tcs = last.tool_calls.as_mut().unwrap();

                            tracing::debug!(
                                "process_sse_line: looking for tool_call id={:?} index={} tcs_len={}",
                                id,
                                index.unwrap_or(0),
                                tcs.len()
                            );

                            // Try to find existing tool call by id first
                            let found = if let Some(ref id) = id {
                                tcs.iter_mut().find(|t| t.id == *id)
                            } else {
                                None
                            };

                            if let Some(tc) = found {
                                // Accumulate args into existing tool call
                                tc.function.arguments.push_str(&args);
                                tracing::debug!(
                                    "process_sse_line: accumulated args for tool_call id={} total_len={} args={:?}",
                                    id.as_ref().unwrap(), tc.function.arguments.len(),
                                    &tc.function.arguments[..tc.function.arguments.len().min(80)]
                                );
                            } else if let Some(idx) = index {
                                // Find by index when id is not present
                                if let Some(tc) = tcs.get_mut(idx as usize) {
                                    tc.function.arguments.push_str(&args);
                                    tracing::debug!(
                                        "process_sse_line: accumulated args for tool_call index={} total_len={} args={:?}",
                                        idx, tc.function.arguments.len(),
                                        &tc.function.arguments[..tc.function.arguments.len().min(80)]
                                    );
                                } else if let Some(ref id) = id {
                                    // Index doesn't exist yet but we have an id - create new tool call
                                    tcs.push(crate::types::ToolCall {
                                        id: id.to_string(),
                                        call_type: "function".to_string(),
                                        function: crate::types::ToolFunction {
                                            name,
                                            arguments: args.clone(),
                                        },
                                    });
                                    tracing::debug!(
                                        "process_sse_line: created new tool_call id={} at index={} args={:?}",
                                        id, idx, args
                                    );
                                } else {
                                    tracing::warn!(
                                        "process_sse_line: failed to find tool_call at index={} (tcs_len={})",
                                        idx, tcs.len()
                                    );
                                }
                            } else if let Some(ref id) = id {
                                // Create new tool call
                                tcs.push(crate::types::ToolCall {
                                    id: id.to_string(),
                                    call_type: "function".to_string(),
                                    function: crate::types::ToolFunction {
                                        name,
                                        arguments: args.clone(),
                                    },
                                });
                                tracing::debug!(
                                    "process_sse_line: created new tool_call id={} args={:?}",
                                    id, args
                                );
                            } else {
                                tracing::warn!(
                                    "process_sse_line: skipping chunk with no id and no index"
                                );
                            }
                            // If no id and no index, skip this chunk
                        } else {
                            tracing::warn!("process_sse_line: no last message in conversation");
                        }
                    }
                }
            }
        }

        // Extract usage from the final chunk (when choices has no delta but has usage)
        if let Some(usage) = chunk.get("usage").and_then(|u| serde_json::from_value(u.clone()).ok()) {
            return Ok(Some(usage));
        }

        Ok(None)
    }

    pub async fn stream_message_with_usage(
        &self,
        prompt: &str,
        callback: impl FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Option<Usage>, Error> {
        self.stream_message_with_tools_and_usage(prompt, None, callback).await
    }

    pub async fn stream_message_with_tools_and_usage(
        &self,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
        mut callback: impl FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Option<Usage>, Error> {
        let request = self.build_request(prompt, true, tools);
        let body = serde_json::to_string(&request)?;

        let mut builder = self
            .http_client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body);
        if let Some(ref key) = self.api_key {
            builder = builder.header(
                "Authorization",
                format!("Bearer {}", key),
            );
        }

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
        {
            let mut conv = self.conversation.lock().unwrap();
            conv.push(Message {
                role: "user".to_string(),
                content: prompt.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            });
            conv.push(Message {
                role: "assistant".to_string(),
                content: String::new(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut last_usage: Option<Usage> = None;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete lines
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if let Some(usage) = Self::process_sse_line(&line, &mut callback, &self.conversation).await? {
                    last_usage = Some(usage);
                }
            }
        }

        Ok(last_usage)
    }

    /// Arc-based streaming method that clones necessary data before calling
    /// the async streaming, avoiding holding a MutexGuard across .await.
    pub async fn stream_message_with_tools_and_usage_arc(
        client: &Arc<Mutex<Self>>,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
        callback: impl FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
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
        tracing::debug!(
            "stream_message (arc) request body:\n{}",
            body
        );

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
        {
            let mut conv = conversation.lock().unwrap();
            conv.push(Message {
                role: "user".to_string(),
                content: prompt.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            });
            conv.push(Message {
                role: "assistant".to_string(),
                content: String::new(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut last_usage: Option<Usage> = None;
        let mut cb = callback;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if line.starts_with("data: ") && !line.contains("[DONE]") {
                    tracing::trace!("stream_message (arc) SSE line: {}", &line["data: ".len()..].chars().take(200).collect::<String>());
                }

                if let Some(usage) = Self::process_sse_line(&line, &mut cb, &conversation).await? {
                    last_usage = Some(usage);
                }
            }
        }

        tracing::debug!("stream_message (arc) streaming completed, usage={:?}", last_usage);
        Ok(last_usage)
    }

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
        Self::execute_pending_tool_calls_arc(&std::sync::Arc::new(std::sync::Mutex::new(self.clone())), tool_manager).await
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
                let _ = tx.send(crate::ui::window::AppEvent::ToolCallStart {
                    tool_name: tc.function.name.clone(),
                    call_id: tc.id.clone(),
                });
            }

            // Parse arguments
            tracing::debug!(
                "execute_pending_tool_calls: parsing args for tool={} call_id={} args={:?}",
                tc.function.name,
                tc.id,
                &tc.function.arguments[..tc.function.arguments.len().min(200)]
            );
            // Try parsing as direct args first, then as wrapped in values
            let params = if let Ok(p) = serde_json::from_str::<crate::tools::ToolParams>(&tc.function.arguments) {
                p
            } else if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                // Model sends direct args like {"expression":"2 + 2"}, wrap them
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
                    let _ = tx.send(crate::ui::window::AppEvent::ToolCallError {
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
                            crate::tools::lib::ToolOutput::Success(v) => v.to_string(),
                            crate::tools::lib::ToolOutput::Error(e) => e.clone(),
                        };
                        let _ = tx.send(crate::ui::window::AppEvent::ToolCallComplete {
                            tool_name: tc.function.name.clone(),
                            call_id: tc.id.clone(),
                            result: result_str,
                        });
                    }
                }
                Err(e) => {
                    if let Some(tx) = client.lock().unwrap().tool_event_tx.lock().unwrap().as_ref() {
                        let _ = tx.send(crate::ui::window::AppEvent::ToolCallError {
                            tool_name: tc.function.name.clone(),
                            call_id: tc.id.clone(),
                            error: e.to_string(),
                        });
                    }
                }
            }

            // Add result message to conversation
            {
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
                    content: result.map_or_else(
                        |e| e.to_string(),
                        |r| match r {
                            crate::tools::lib::ToolOutput::Success(v) => v.to_string(),
                            crate::tools::lib::ToolOutput::Error(e) => e,
                        }
                    ),
                    timestamp: String::new(),
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
        let request = client.build_request("Hello", false, None);
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
        let request = client.build_request("Hello", false, None);
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
        let request = client.build_request("Hello", true, None);
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
        let request = client.build_request("Hello", false, Some(&tools));
        assert!(request.tools.is_some());
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_build_request_includes_history() {
        let client = ChatClient::new("http://localhost:8080");
        {
            let mut conv = client.conversation().lock().unwrap();
            conv.push(Message { role: "user".into(), content: "Hi there".into(), timestamp: String::new(), tool_calls: None });
            conv.push(Message { role: "assistant".into(), content: "Hello! How can I help?".into(), timestamp: String::new(), tool_calls: None });
        }
        let request = client.build_request("What's the weather?", false, None);
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
            let mut conv = client.conversation().lock().unwrap();
            conv.push(Message { role: "user".into(), content: "Hi".into(), timestamp: String::new(), tool_calls: None });
            conv.push(Message { role: "assistant".into(), content: String::new(), timestamp: String::new(), tool_calls: None });
        }
        let request = client.build_request("Follow up", false, None);
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
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = ChatClient::process_sse_line("", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_sse_line_done() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = ChatClient::process_sse_line("data: [DONE]", &mut cb, &client.conversation()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_non_data() {
        let client = ChatClient::new("http://localhost:8080");
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = ChatClient::process_sse_line("id: 1", &mut cb, &client.conversation()).await;
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
        });
        drop(conv);

        let sse_data = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = ChatClient::process_sse_line(sse_data, &mut cb, &client.conversation()).await;
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
        });
        drop(conv);

        let sse1 = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        ChatClient::process_sse_line(sse1, &mut cb, &client.conversation()).await.unwrap();

        let sse2 = r#"data: {"choices":[{"delta":{"content":" world"}}]}"#;
        let mut captured2 = Vec::new();
        let mut cb2 = |s: String| -> Result<(), Error> {
            captured2.push(s);
            Ok(())
        };
        ChatClient::process_sse_line(sse2, &mut cb2, &client.conversation()).await.unwrap();

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
        });
        drop(conv);

        let sse_data = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let result = ChatClient::process_sse_line(sse_data, &mut cb, &client.conversation()).await;
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
        });
        drop(conv);

        let warnings = client.check_tool_call_warnings();
        assert!(warnings.is_empty());
    }
}
