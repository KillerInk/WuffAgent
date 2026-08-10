use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use tracing;

use tokio::task::JoinHandle;

use super::{ChatClient, Error as ClientError};
use crate::tools::manager::ToolManager;
use crate::types::Usage;
use crate::ui::window::AppEvent;

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
            max_tool_rounds: 10,
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
    let mut tools = initial_tools.map(|t| t.to_vec());
    let mut round = 0;

    tracing::info!(
        "run_chat_loop starting, round=0 prompt={} tool_count={}",
        current_prompt,
        tools.as_ref().map(|t| t.len()).unwrap_or(0)
    );

    loop {
        tracing::debug!(
            "run_chat_loop iteration round={} prompt={} tool_count={}",
            round,
            current_prompt,
            tools.as_ref().map(|t| t.len()).unwrap_or(0)
        );

        // 1. Stream the request, forwarding chunks as events
        let (content, usage, has_tool_calls) =
            stream_request(client, &current_prompt, tools.as_ref().map(|t| t.as_slice()), event_tx)
                .await?;

        tracing::debug!(
            "run_chat_loop round={} stream done, content_len={} has_tool_calls={}",
            round,
            content.len(),
            has_tool_calls
        );

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

        // 4. Execute pending tool calls — must not hold MutexGuard across .await
        {
            tracing::debug!("run_chat_loop round={} executing pending tool calls", round);
            let client_clone = client.clone();
            let tm = tool_manager.clone();
            // Call the method directly on the Arc<Mutex<ChatClient>> so we
            // can drop the guard before the async operation.
            let result = crate::client::ChatClient::execute_pending_tool_calls_arc(&client_clone, &tm).await;
            result?;
        }

        // 5. Check if more tool calls are pending
        {
            let has_more = {
                let guard = client.lock().unwrap();
                guard.has_pending_tool_calls()
            };
            if !has_more {
                // Get the final content from the last assistant message
                let final_content = {
                    let guard = client.lock().unwrap();
                    guard
                        .get_last_assistant_message()
                        .map(|m| m.content)
                        .unwrap_or_default()
                };
                tracing::info!(
                    "run_chat_loop round={} loop exit (no more tool calls), final_content_len={}",
                    round,
                    final_content.len()
                );
                event_tx.send(EngineEvent::StreamComplete {
                    content: final_content,
                    usage,
                })?;
                return Ok(());
            }
        }

        // 6. Max rounds check
        round += 1;
        if round >= config.max_tool_rounds {
            tracing::warn!(
                "run_chat_loop exiting: max rounds ({}) reached after {} iterations",
                config.max_tool_rounds,
                round
            );
            return Err(EngineError::MaxRounds(config.max_tool_rounds));
        }

        // 7. Continue with "Continue" prompt, don't re-send tools
        current_prompt = "Continue".to_string();
        tools = None;
    }
}

/// Stream a request and forward chunks as EngineEvent::StreamChunk.
/// Returns (accumulated_content, usage, has_tool_calls).
async fn stream_request(
    client: &Arc<Mutex<ChatClient>>,
    prompt: &str,
    tools: Option<&[crate::tools::ToolDefinition]>,
    event_tx: &mpsc::Sender<EngineEvent>,
) -> Result<(String, Option<Usage>, bool), EngineError> {
    let streamed_content = Arc::new(Mutex::new(String::new()));
    let streamed_content_clone = streamed_content.clone();
    let event_tx_clone = event_tx.clone();

    let callback = move |chunk: String| -> Result<(), ClientError> {
        if !chunk.is_empty() {
            let mut content = streamed_content_clone.lock().unwrap();
            content.push_str(&chunk);
            drop(content);

            let _ = event_tx_clone.send(EngineEvent::StreamChunk {
                content: chunk,
            });
        }
        Ok(())
    };

    // Clone the client Arc so we own it in the callback
    let client_clone = client.clone();
    let prompt = prompt.to_string();
    let tools_clone = tools.map(|t| t.to_vec());

    tracing::debug!(
        "stream_request starting, prompt={} tool_count={}",
        prompt,
        tools_clone.as_ref().map(|t| t.len()).unwrap_or(0)
    );

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
    Ok((content, usage_from_stream, has_tool_calls))
}

/// Convert EngineEvent to AppEvent for UI
impl From<EngineEvent> for AppEvent {
    fn from(event: EngineEvent) -> Self {
        match event {
            EngineEvent::StreamChunk { content } => AppEvent::StreamChunk { content },
            EngineEvent::StreamComplete { content, usage } => {
                AppEvent::StreamComplete { content, usage }
            }
            EngineEvent::StreamError { error } => AppEvent::StreamError { error },
            EngineEvent::ToolCallStart { tool_name, call_id } => {
                AppEvent::ToolCallStart { tool_name, call_id }
            }
            EngineEvent::ToolCallComplete {
                tool_name,
                call_id,
                result,
            } => AppEvent::ToolCallComplete {
                tool_name,
                call_id,
                result,
            },
            EngineEvent::ToolCallError {
                tool_name,
                call_id,
                error,
            } => AppEvent::ToolCallError {
                tool_name,
                call_id,
                error,
            },
        }
    }
}
