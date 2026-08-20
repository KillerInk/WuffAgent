use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use tracing;

use tokio::task::JoinHandle;

use super::{ChatClient, Error as ClientError};
use crate::tools::manager::ToolManager;
use crate::types::Usage;

/// Errors that can occur during chat engine operations
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("Tool execution error: {0}")]
    Tool(String),
    #[error("Max tool rounds ({0}) reached")]
    MaxRounds(usize),
    #[error("Send error: {0}")]
    Send(#[from] mpsc::SendError<EngineEvent>),
    #[error("Client error: {0}")]
    Client(#[from] ClientError),
}

/// Events that the ChatEngine can produce
#[derive(Debug)]
pub enum EngineEvent {
    /// A new stream chunk arrived
    StreamChunk { content: String },
    /// Streaming is complete
    StreamComplete { content: String, usage: Option<Usage> },
    /// An error occurred
    StreamError { error: String },
    /// A tool call started
    ToolCallStart { tool_name: String, call_id: String },
    /// A tool call completed
    ToolCallComplete { tool_name: String, call_id: String, result: String },
    /// A tool call errored
    ToolCallError { tool_name: String, call_id: String, error: String },
    /// A chunk of model thinking/reasoning content (e.g. Claude-style)
    ThinkingChunk { content: String },
    /// Thinking/reasoning phase is complete
    ThinkingComplete { content: String },
}

/// Configuration for the chat engine
#[derive(Clone)]
pub struct EngineConfig {
    /// Timeout for sending messages (seconds)
    pub send_timeout_secs: u64,
    /// Timeout for tool execution (seconds)
    pub tool_timeout_secs: u64,
    /// Maximum tool call rounds before giving up
    pub max_tool_rounds: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            send_timeout_secs: 60,
            tool_timeout_secs: 30,
            max_tool_rounds: 20,
        }
    }
}

/// ChatEngine handles all async communication with the model and tool execution.
/// It centralizes the async logic to avoid nested runtimes and Send-safety issues.
pub struct ChatEngine {
    client: Arc<Mutex<ChatClient>>,
    tool_manager: ToolManager,
    config: EngineConfig,
    event_tx: mpsc::Sender<EngineEvent>,
    /// Handle to the running task (for cancellation)
    task_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Clone for ChatEngine {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            tool_manager: self.tool_manager.clone(),
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            task_handle: std::sync::Mutex::new(None),
        }
    }
}

impl ChatEngine {
    pub fn new(
        client: Arc<Mutex<ChatClient>>,
        tool_manager: ToolManager,
        event_tx: mpsc::Sender<EngineEvent>,
    ) -> Self {
        Self {
            client,
            tool_manager,
            config: EngineConfig::default(),
            event_tx,
            task_handle: std::sync::Mutex::new(None),
        }
    }

    /// Set configuration
    pub fn with_config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// Start a chat session with the given prompt
    pub fn start_chat(&self, prompt: String, tool_defs: Vec<crate::tools::ToolDefinition>) {
        // Cancel any existing task
        self.cancel();

        let client = self.client.clone();
        let tool_manager = self.tool_manager.clone();
        let event_tx = self.event_tx.clone();
        let config = self.config.clone();

        let handle = tokio::spawn(async move {
            let result = run_chat_loop(
                &client,
                &tool_manager,
                &event_tx,
                &prompt,
                Some(&tool_defs),
                config,
            )
            .await;

            if let Err(e) = result {
                let _ = event_tx.send(EngineEvent::StreamError {
                    error: e.to_string(),
                });
            } else {
                // Notify the UI that the chat loop finished
                let _ = event_tx.send(EngineEvent::StreamComplete { content: String::new(), usage: None });
            }
        });

        *self.task_handle.lock().unwrap() = Some(handle);
    }

    /// Stop the current chat session
    pub fn cancel(&self) {
        if let Some(handle) = self.task_handle.lock().unwrap().take() {
            handle.abort();
        }
    }
}

/// Run the chat loop: send message, execute tools if needed, repeat
async fn run_chat_loop(
    client: &Arc<Mutex<ChatClient>>,
    tool_manager: &ToolManager,
    event_tx: &mpsc::Sender<EngineEvent>,
    prompt: &str,
    initial_tools: Option<&[crate::tools::ToolDefinition]>,
    config: EngineConfig,
) -> Result<(), EngineError> {
    let mut current_prompt = prompt.to_string();
    let tools = initial_tools.map(|t| t.to_vec());
    let mut round = 0;

    loop {

        // 1. Stream the request, forwarding chunks as events
        let (content, usage, has_tool_calls, thinking_content) =
            stream_request(client, &current_prompt, tools.as_deref(), event_tx)
                .await?;

        // 1.5. Emit thinking complete event if there was thinking content
        if !thinking_content.is_empty() {
            tracing::info!(
                "run_chat_loop round={} thinking complete, thinking_len={}",
                round,
                thinking_content.len()
            );
            event_tx.send(EngineEvent::ThinkingComplete { content: thinking_content })?;
        }

        // 2. Check for tool calls in the response
        if !has_tool_calls {
            tracing::info!(
                "run_chat_loop round={} normal completion, final_content_len={}",
                round,
                content.len()
            );
            event_tx.send(EngineEvent::StreamComplete { content, usage })?;
            return Ok(());
        }

        // 2.5. Emit StreamComplete for the current content/response so far, before tool execution
        // This allows the UI to show the full content up to this point.
        event_tx.send(EngineEvent::StreamComplete { content: content.clone(), usage: usage.clone() })?;

        // 3. Validate tool calls before executing
        {
            let warnings = {
                let guard = client.lock().unwrap();
                guard.check_tool_call_warnings()
            };
            if !warnings.is_empty() {
                tracing::warn!(
                    "run_chat_loop round={} tool call warnings: {:?}",
                    round,
                    warnings
                );
            }
            for (name, msg) in warnings {
                event_tx.send(EngineEvent::ToolCallError {
                    tool_name: name,
                    call_id: String::new(),
                    error: msg,
                })?;
            }
        }

        // 3. Execute pending tool calls — must not hold MutexGuard across .await
        {
            tracing::debug!("run_chat_loop round={} executing pending tool calls", round);
            let client_clone = client.clone();
            let tm = tool_manager.clone();
            // Call the method directly on the Arc<Mutex<ChatClient>> so we
            // can drop the guard before the async operation.
            let result = crate::client::ChatClient::execute_pending_tool_calls_arc(&client_clone, &tm).await;
            result?;
        }

        // 4. Execute succeeded — continue loop to send tool results back to LLM.
        // The LLM will see the tool results and decide next steps (more tool calls
        // or final response). We only exit when the LLM responds without tool calls.

        // 5. Max rounds check
        round += 1;
        if round >= config.max_tool_rounds {
            tracing::warn!(
                "run_chat_loop exiting: max rounds ({}) reached after {} iterations",
                config.max_tool_rounds,
                round
            );
            // Send the last content as complete instead of error
            event_tx.send(EngineEvent::StreamComplete { content, usage })?;
            return Ok(());
        }

        // 6. Continue with explicit prompt to keep using tools if needed
        current_prompt = "Please continue with your task. Use tools if needed, otherwise provide your final response.".to_string();
        
    }
}

/// Stream a request and forward chunks as EngineEvent::StreamChunk.
/// Returns (accumulated_content, usage, has_tool_calls, accumulated_thinking).
async fn stream_request(
    client: &Arc<Mutex<ChatClient>>,
    prompt: &str,
    tools: Option<&[crate::tools::ToolDefinition]>,
    event_tx: &mpsc::Sender<EngineEvent>,
) -> Result<(String, Option<Usage>, bool, String), EngineError> {
    let streamed_content = Arc::new(Mutex::new(String::new()));
    let streamed_content_clone = streamed_content.clone();
    let thinking_content = Arc::new(Mutex::new(String::new()));
    let thinking_content_clone = thinking_content.clone();
    let event_tx_clone = event_tx.clone();

    let callback = move |chunk: String, is_thinking: bool| -> Result<(), ClientError> {
        if !chunk.is_empty() {
            if is_thinking {
                let mut thinking = thinking_content_clone.lock().unwrap();
                thinking.push_str(&chunk);
                drop(thinking);

                let _ = event_tx_clone.send(EngineEvent::ThinkingChunk {
                    content: chunk,
                });
            } else {
                let mut content = streamed_content_clone.lock().unwrap();
                content.push_str(&chunk);
                drop(content);

                let _ = event_tx_clone.send(EngineEvent::StreamChunk {
                    content: chunk,
                });
            }
        }
        Ok(())
    };

    // Clone the client Arc so we own it in the callback
    let client_clone = client.clone();
    let prompt = prompt.to_string();
    let tools_clone = tools.map(|t| t.to_vec());

    // Call the streaming method using the Arc-based version to avoid
    // holding a MutexGuard across .await (MutexGuard is not Send)
    let usage_from_stream = ChatClient::stream_message_with_tools_and_usage_arc(
        &client_clone,
        &prompt,
        tools_clone.as_deref(),
        callback,
    )
    .await?;

    // Determine if there are tool calls in the last assistant message
    let has_tool_calls = {
        let guard = client.lock().unwrap();
        guard.has_pending_tool_calls()
    };

    tracing::debug!(
        "stream_request done, has_tool_calls={}, usage={:?}",
        has_tool_calls,
        usage_from_stream
    );

    let content = streamed_content.lock().unwrap().clone();
    let thinking = thinking_content.lock().unwrap().clone();
    Ok((content, usage_from_stream, has_tool_calls, thinking))
}

