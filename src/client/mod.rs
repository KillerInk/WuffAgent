use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Serialize, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
}

#[derive(Deserialize, Debug)]
pub struct Response {
    pub choices: Vec<Choice>,
}

#[derive(Deserialize, Debug)]
pub struct Choice {
    pub message: Message,
}

pub struct ChatClient {
    pub base_url: String,
    pub system_prompt: String,
    pub conversation: Arc<Mutex<Vec<Message>>>,
    pub http_client: reqwest::Client,
    pub api_key: Option<String>,
}

impl ChatClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            system_prompt: String::new(),
            conversation: Arc::new(Mutex::new(Vec::new())),
            http_client: reqwest::Client::new(),
            api_key: None,
        }
    }

    pub fn set_url(&mut self, url: &str) {
        self.base_url = url.to_string();
    }

    pub fn set_api_key(&mut self, key: Option<&str>) {
        self.api_key = key.map(|s| s.to_string());
    }

    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Content-Type", "application/json".parse().unwrap());
        if let Some(ref key) = self.api_key {
            headers.insert(
                "Authorization",
                format!("Bearer {}", key).parse().unwrap(),
            );
        }
        headers
    }

    fn build_request(system_prompt: &str, prompt: &str, stream: bool) -> ChatRequest {
        let mut messages = Vec::new();

        if !system_prompt.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        });

        ChatRequest {
            model: "local".to_string(),
            messages,
            stream,
        }
    }

    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_prompt = prompt.to_string();
    }

    pub fn clear_history(&self) {
        self.conversation.lock().unwrap().clear();
    }

    pub async fn send_message(
        base_url: &str,
        system_prompt: &str,
        conversation: Arc<Mutex<Vec<Message>>>,
        http_client: &reqwest::Client,
        api_key: Option<&str>,
        prompt: &str,
    ) -> Result<String, Error> {
        let request = Self::build_request(system_prompt, prompt, false);
        let body = serde_json::to_string(&request)?;

        let mut builder = http_client
            .post(format!("{}/v1/chat/completions", base_url))
            .header("Content-Type", "application/json")
            .body(body);
        if let Some(ref key) = api_key {
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

        // Update conversation history
        let mut conv = conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
        });
        drop(conv);

        Ok(content)
    }

    pub async fn stream_message(
        base_url: &str,
        system_prompt: &str,
        conversation: Arc<Mutex<Vec<Message>>>,
        http_client: &reqwest::Client,
        api_key: Option<&str>,
        prompt: &str,
        mut callback: impl FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<(), Error> {
        let request = Self::build_request(system_prompt, prompt, true);
        let body = serde_json::to_string(&request)?;

        let mut builder = http_client
            .post(format!("{}/v1/chat/completions", base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body);
        if let Some(ref key) = api_key {
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
            let mut conv = conversation.lock().unwrap();
            conv.push(Message {
                role: "user".to_string(),
                content: prompt.to_string(),
            });
            conv.push(Message {
                role: "assistant".to_string(),
                content: String::new(),
            });
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete lines
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                Self::process_sse_line(&line, &mut callback, conversation.clone()).await?;
            }
        }

        Ok(())
    }

    async fn process_sse_line(
        line: &str,
        callback: &mut impl FnMut(String) -> Result<(), Error>,
        conversation: Arc<Mutex<Vec<Message>>>,
    ) -> Result<(), Error> {
        if line.is_empty() || line == "data: [DONE]" {
            return Ok(());
        }

        if !line.starts_with("data: ") {
            return Ok(());
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

        Ok(())
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
        let request = ChatClient::build_request("", "Hello", false);
        assert_eq!(request.model, "local");
        assert!(!request.stream);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].content, "Hello");
    }

    #[test]
    fn test_build_request_with_system_prompt() {
        let request = ChatClient::build_request("You are helpful.", "Hello", false);
        assert_eq!(request.model, "local");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[0].content, "You are helpful.");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "Hello");
    }

    #[test]
    fn test_build_request_streaming() {
        let request = ChatClient::build_request("", "Hello", true);
        assert!(request.stream);
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_chat_client_api_key() {
        let mut client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.api_key, None);
        client.set_api_key(Some("sk-test"));
        assert_eq!(client.api_key, Some("sk-test".to_string()));
    }

    #[test]
    fn test_chat_client_base_url() {
        let client = ChatClient::new("http://localhost:8080");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[tokio::test]
    async fn test_process_sse_line_empty() {
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let conv = Arc::new(Mutex::new(Vec::new()));
        let result = ChatClient::process_sse_line("", &mut cb, conv.clone()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_sse_line_done() {
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let conv = Arc::new(Mutex::new(Vec::new()));
        let result = ChatClient::process_sse_line("data: [DONE]", &mut cb, conv.clone()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_non_data() {
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let conv = Arc::new(Mutex::new(Vec::new()));
        let result = ChatClient::process_sse_line("id: 1", &mut cb, conv.clone()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }

    #[tokio::test]
    async fn test_process_sse_line_valid_chunk() {
        let sse_data = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let conv = Arc::new(Mutex::new(Vec::new()));
        // Pre-seed an empty assistant message (caller does this in stream_message)
        conv.lock().unwrap().push(Message {
            role: "assistant".to_string(),
            content: String::new(),
        });
        let result = ChatClient::process_sse_line(sse_data, &mut cb, conv.clone()).await;
        assert!(result.is_ok());
        assert_eq!(captured, vec!["Hello"]);
        let c = conv.lock().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].role, "assistant");
        assert_eq!(c[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_process_sse_line_multiple_chunks() {
        let conv = Arc::new(Mutex::new(Vec::new()));
        conv.lock().unwrap().push(Message {
            role: "assistant".to_string(),
            content: String::new(),
        });

        let sse1 = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        ChatClient::process_sse_line(sse1, &mut cb, conv.clone()).await.unwrap();

        let sse2 = r#"data: {"choices":[{"delta":{"content":" world"}}]}"#;
        let mut captured2 = Vec::new();
        let mut cb2 = |s: String| -> Result<(), Error> {
            captured2.push(s);
            Ok(())
        };
        ChatClient::process_sse_line(sse2, &mut cb2, conv.clone()).await.unwrap();

        assert_eq!(captured, vec!["Hello"]);
        assert_eq!(captured2, vec![" world"]);
        let c = conv.lock().unwrap();
        assert_eq!(c[0].content, "Hello world");
    }

    #[tokio::test]
    async fn test_process_sse_line_empty_content() {
        let sse_data = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        let mut captured = Vec::new();
        let mut cb = |s: String| -> Result<(), Error> {
            captured.push(s);
            Ok(())
        };
        let conv = Arc::new(Mutex::new(Vec::new()));
        conv.lock().unwrap().push(Message {
            role: "assistant".to_string(),
            content: String::new(),
        });
        let result = ChatClient::process_sse_line(sse_data, &mut cb, conv.clone()).await;
        assert!(result.is_ok());
        assert!(captured.is_empty());
    }
}
