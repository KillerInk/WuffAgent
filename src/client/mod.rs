use futures::StreamExt;
use serde::{Deserialize, Serialize};
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
        }
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

    pub fn load_session(&mut self) -> Option<crate::sessions::Session> {
        let dir = self.session_dir.clone();
        let id = self.session_id.as_ref()?;
        let session = crate::sessions::load_session(&dir, id)?;
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
        let session = crate::sessions::load_session(&self.session_dir, id)
            .ok_or_else(|| anyhow::anyhow!("session not found"))?;
        let mut updated = session.clone();
        updated.messages = conv.clone();
        crate::sessions::save_session(&self.session_dir, &updated)
    }

    fn build_request(&self, prompt: &str, stream: bool, tools: Option<&[crate::tools::ToolDefinition]>) -> ChatRequest {
        let mut messages = Vec::new();

        if !self.system_prompt.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: self.system_prompt.clone(),
                tool_calls: None,
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            tool_calls: None,
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

        let response: Response = serde_json::from_str(&text)?;

        if response.choices.is_empty() {
            return Err(Error::Stream("Empty response".to_string()));
        }

        let content = response.choices[0].message.content.clone();
        let usage = response.usage.clone();

        // Update conversation history
        let mut conv = self.conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            tool_calls: None,
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            tool_calls: None,
        });
        drop(conv);

        self.trim_conversation(100);

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
                if let Some(first) = tc_array.first() {
                    if let (Some(id), Some(func)) = (
                        first.get("id").and_then(|v| v.as_str()),
                        first.get("function"),
                    ) {
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

                        // Accumulate partial args from streaming
                        let mut conv = conversation.lock().unwrap();
                        if let Some(last) = conv.last_mut() {
                            if last.tool_calls.is_none() {
                                last.tool_calls = Some(Vec::new());
                            }
                            let tcs = last.tool_calls.as_mut().unwrap();
                            if let Some(tc) = tcs.iter_mut().find(|t| t.id == id) {
                                tc.function.arguments.push_str(&args);
                            } else {
                                tcs.push(crate::types::ToolCall {
                                    id: id.to_string(),
                                    call_type: "function".to_string(),
                                    function: crate::types::ToolFunction {
                                        name,
                                        arguments: args,
                                    },
                                });
                            }
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
                tool_calls: None,
            });
            conv.push(Message {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: None,
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
}
