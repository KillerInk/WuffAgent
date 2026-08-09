# Phase 4: Chat Client (Rust)

## Status: Pending

---

### Step 4.1: HTTP Request Builder

**Objective**: Build HTTP requests for chat completions.

**Tasks**:
- Create `src/client/mod.rs`
- Define `Message`, `ChatRequest`, `Response` structs with serde
- Implement `build_request()` method
- Serialize to JSON, create HTTP request with reqwest

```rust
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

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
    base_url: String,
    system_prompt: String,
    conversation: Arc<Mutex<Vec<Message>>>,
    http_client: reqwest::Client,
    abort_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<()>>>>,
}

impl ChatClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            system_prompt: String::new(),
            conversation: Arc::new(Mutex::new(Vec::new())),
            http_client: reqwest::Client::new(),
            abort_sender: Arc::new(Mutex::new(None)),
        }
    }

    fn build_request(&self, prompt: &str, stream: bool) -> ChatRequest {
        let mut messages = Vec::new();
        
        if !self.system_prompt.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: self.system_prompt.clone(),
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

    pub async fn clear_history(&self) {
        self.conversation.lock().await.clear();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Stream error: {0}")]
    Stream(String),
}
```

**Success Criteria**:
- JSON matches OpenAI API format
- Request includes system prompt, message history

**Dependencies**: Step 2.1 (config for base URL)

---

### Step 4.2: Non-Streaming Response Handling

**Objective**: Send request, receive complete response.

**Tasks**:
- Implement `send_message()` with `stream: false`
- Parse JSON response
- Return content to caller
- Update conversation history

```rust
impl ChatClient {
    pub async fn send_message(&self, prompt: &str) -> Result<String, Error> {
        let request = self.build_request(prompt, false);
        let body = serde_json::to_string(&request)?;
        
        let resp = self.http_client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            return Err(Error::Http(reqwest::Error::from(reqwest::Response::from(
                http::Response::builder().status(status).body(text).unwrap()
            ))));
        }
        
        let response: Response = serde_json::from_str(&text)?;
        
        if response.choices.is_empty() {
            return Err(Error::Stream("Empty response".to_string()));
        }
        
        let content = response.choices[0].message.content.clone();
        
        // Update conversation history
        let mut conv = self.conversation.lock().await;
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
        });
        
        Ok(content)
    }
}
```

**Success Criteria**:
- Complete response text is returned
- Error handling for HTTP failures
- Conversation history is updated

**Dependencies**: Step 4.1

---

### Step 4.3: SSE Streaming

**Objective**: Stream tokens as they arrive.

**Tasks**:
- Implement `stream_message()` with `stream: true`
- Parse SSE `data:` lines using reqwest streaming
- Call callback for each token chunk
- Use async callback pattern
- Support cancellation via tokio abort
- Handle `[DONE]` marker

```rust
use std::pin::Pin;
use std::task::{Context, Poll};
use futures::StreamExt;

type StreamCallback = Box<dyn FnMut(String) -> Result<(), Error> + Send + Sync>;

impl ChatClient {
    pub async fn stream_message<F>(&self, prompt: &str, mut callback: F) -> Result<(), Error>
    where
        F: FnMut(String) -> Result<(), Error> + Send + Sync + 'static,
    {
        let request = self.build_request(prompt, true);
        let body = serde_json::to_string(&request)?;
        
        let resp = self.http_client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body)
            .send()
            .await?;
        
        if !resp.status().is_success() {
            return Err(Error::Http(reqwest::Error::from(resp)));
        }
        
        // Add user message to history
        let mut conv = self.conversation.lock().await;
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: String::new(),
        });
        drop(conv);
        
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            
            // Process complete lines
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();
                
                self.process_sse_line(&line, &mut callback).await?;
            }
        }
        
        Ok(())
    }
    
    async fn process_sse_line(&self, line: &str, callback: &mut impl FnMut(String) -> Result<(), Error>) -> Result<(), Error> {
        if line.is_empty() || line == "data: [DONE]" {
            return Ok(());
        }
        
        if !line.starts_with("data: ") {
            return Ok(());
        }
        
        let data = &line["data: ".len()..];
        let chunk: serde_json::Value = serde_json::from_str(data)?;
        
        if let Some(text) = chunk.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
        {
            if !text.is_empty() {
                callback(text.to_string())?;
                
                // Update conversation history
                let mut conv = self.conversation.lock().await;
                if let Some(last) = conv.last_mut() {
                    last.content.push_str(text);
                }
            }
        }
        
        Ok(())
    }
    
    pub async fn stop_generation(&self) {
        if let Some(sender) = self.abort_sender.lock().await.take() {
            let _ = sender.send(()).await;
        }
    }
}
```

**Success Criteria**:
- Callback fires for each token
- Streaming completes on `[DONE]` marker
- Can cancel streaming with abort

**Dependencies**: Step 4.1

---

## Files Created:
- `src/client/mod.rs`

## Dependencies on other phases:
- Phase 2 (config provides base URL)
- Phase 5 (UI needs streaming updates)

## Review Notes:
- `reqwest` with streaming feature for SSE
- `futures::StreamExt` for byte stream handling
- Buffer-based line parsing for SSE
- JSON parsing per SSE event
- Conversation history updated incrementally during streaming
- `thiserror` for error types (add to Cargo.toml)
- `http` crate may be needed for error conversion (add to Cargo.toml)
- Callback pattern allows flexible UI integration
