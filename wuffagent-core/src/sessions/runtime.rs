use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::agents::AgentEngine;
use crate::client::ChatPipeline;

/// A message sent while the AI is still working. Displayed in the chat
/// immediately and processed as the next turn once the current run (and any
/// earlier queued messages) finishes.
#[derive(Clone)]
pub struct QueuedMessage {
    pub text: String,
    pub image: Option<egui::ImageSource<'static>>,
    /// System prompt resolved from the selected agent at send time.
    pub agent_prompt: String,
    /// Tool policy (allowed_tools + shell config) resolved from the selected agent.
    pub tool_policy: crate::client::pipeline::ChatToolPolicy,
}

/// State for the chat area (per-session UI display state).
#[derive(Clone)]
pub struct ChatAreaState {
    pub messages: Vec<crate::types::ChatMessage>,
    pub input_text: String,
    pub stream_buffer: String,
    pub pending_error: Option<String>,
    pub is_generating: bool,
    pub scroll_to_bottom_requested: bool,
    pub current_thinking: String,
    pub at_bottom: bool,
    pub button_opacity: f32,
    pub button_visible: bool,
    pub editing_message_index: Option<usize>,
    pub editing_message_content: String,
    pub expanded_messages: Vec<usize>,
    pub context_used: f32,
    pub token_count: usize,
    pub pending_image: Option<egui::ImageSource<'static>>,
    /// Status shown in the status bar for this session.
    pub status: crate::types::AppStatus,
    /// Messages queued while this session's run was still active.
    pub queued_messages: Vec<QueuedMessage>,
}

impl Default for ChatAreaState {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            input_text: String::new(),
            stream_buffer: String::new(),
            pending_error: None,
            is_generating: false,
            scroll_to_bottom_requested: false,
            current_thinking: String::new(),
            at_bottom: true,
            button_opacity: 0.0,
            button_visible: false,
            editing_message_index: None,
            editing_message_content: String::new(),
            expanded_messages: Vec::new(),
            context_used: 0.0,
            token_count: 0,
            pending_image: None,
            status: crate::types::AppStatus::Stopped,
            queued_messages: Vec::new(),
        }
    }
}

impl ChatAreaState {
    pub fn push_message(&mut self, kind: crate::types::MessageKind, role: &str, content: &str) {
        self.messages.push(crate::types::ChatMessage {
            kind,
            role: role.to_string(),
            content: content.to_string(),
            timestamp: crate::types::format_timestamp(),
            image: None,
        });
    }

    pub fn append_message(&mut self, role: &str, content: &str) {
        self.push_message(crate::types::MessageKind::Normal, role, content);
    }

    pub fn stream_chunk(&mut self, chunk: &str) {
        self.stream_buffer.push_str(chunk);
    }

    pub fn commit_stream(&mut self) {
        let buffer = std::mem::take(&mut self.stream_buffer);
        if !buffer.is_empty() {
            self.append_message("assistant", &buffer);
        }
    }

    /// Show a brief notification message (stored as a temporary system message).
    pub fn show_notification(&mut self, msg: &str, _success: bool) {
        self.messages.push(crate::types::ChatMessage {
            kind: crate::types::MessageKind::Normal,
            role: "system".to_string(),
            content: format!("⚡ {}", msg),
            timestamp: crate::types::format_timestamp(),
            image: None,
        });
    }

    /// Populate the chat display from the client's current conversation
    /// (e.g. after loading a persisted session from disk). Preserves each
    /// message's stored timestamp and renders tool results / reasoning
    /// content with their proper display kinds.
    pub fn reload_messages_from_client(
        &mut self,
        conversation: &std::sync::Arc<std::sync::Mutex<Vec<crate::types::Message>>>,
    ) {
        self.messages.clear();
        let conv = conversation.lock().unwrap();
        for msg in conv.iter() {
            let ts = if msg.timestamp.is_empty() {
                crate::types::format_timestamp()
            } else {
                msg.timestamp.clone()
            };
            if msg.role == "system" {
                continue; // system prompt is not a chat display message
            }
            let kind = if msg.role == "tool" {
                crate::types::MessageKind::Tool
            } else {
                crate::types::MessageKind::Normal
            };
            // Surface stored reasoning as its own dim/italic message,
            // ordered before the assistant text it belongs to.
            if let Some(ref thinking) = msg.reasoning_content {
                if !thinking.is_empty() {
                    self.messages.push(crate::types::ChatMessage {
                        kind: crate::types::MessageKind::Thinking,
                        role: msg.role.clone(),
                        content: thinking.clone(),
                        timestamp: ts.clone(),
                        image: None,
                    });
                }
            }
            self.messages.push(crate::types::ChatMessage {
                kind,
                role: msg.role.clone(),
                content: msg.content.clone(),
                timestamp: ts,
                image: None,
            });
        }
        drop(conv);
        // Jump the view to the bottom of the loaded history on next frame.
        self.scroll_to_bottom_requested = true;
    }
}

/// Per-session runtime state.
///
/// Each session runs independently with its own conversation, pipeline,
/// engine, and cancellation token. The engine is built from the session's own
/// client so that each session's agent chat loop reads/writes an isolated
/// conversation (the shared bootstrap engine's client would otherwise be
/// mutated by every session in parallel).
///
/// All events (stream chunks, tool calls, completions) carry a `session_id`
/// and flow through the app's shared event channel, so this runtime holds no
/// per-session event channel of its own.
pub struct SessionRuntime {
    /// Unique session identifier.
    pub session_id: String,
    /// Session name (displayed in UI).
    pub name: String,
    /// Per-session chat client with isolated conversation history.
    pub client: crate::client::ChatClient,
    /// Per-session chat pipeline for running tasks.
    pub pipeline: ChatPipeline,
    /// Per-session agent engine, built from this session's own client so the
    /// agent chat loop runs against an isolated conversation store.
    pub engine: AgentEngine,
    /// Per-session cancellation token.
    pub cancel_token: CancellationToken,
    /// The agent profile name selected for this session (changeable). Resolved
    /// to a system prompt + tool policy at send time; `None` until the UI
    /// picks one (new sessions default to a profile in the egui layer).
    pub selected_agent: Option<String>,
    /// Chat area state for UI display.
    pub chat_state: ChatAreaState,
}

impl SessionRuntime {
    /// Create a new session runtime.
    pub fn new(
        session_id: String,
        name: String,
        client: crate::client::ChatClient,
        pipeline: ChatPipeline,
        engine: AgentEngine,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session_id,
            name,
            client,
            pipeline,
            engine,
            cancel_token,
            selected_agent: None,
            chat_state: ChatAreaState::default(),
        }
    }

    /// Build a full per-session runtime from the app-level config: a session
    /// client (request options, encryption, session binding, tool event
    /// routing; URL + API key come from the shared `connection` settings so
    /// settings/preset changes reach it), a per-session engine cloned from
    /// to this session's client (so each session's agent chat loop reads/writes
    /// an isolated conversation store), a pipeline carrying the session ID for
    /// event routing, and a fresh cancellation token.
    ///
    /// If the session file exists on disk it is loaded into the client and its
    /// on-disk name wins (the caller's `name` is only a fallback for
    /// brand-new sessions that have no persisted file yet).
    pub fn create_from_config(
        config: &crate::config::Config,
        connection: &crate::client::ConnectionSettings,
        template_engine: &AgentEngine,
        session_id: String,
        name: String,
        event_tx: std::sync::mpsc::Sender<crate::types::AppEvent>,
    ) -> Self {
        let mut client = crate::client::ChatClient::from_settings(connection.clone());
        client.set_session(Some(session_id.clone()), config.sessions_dir.clone());
        client.set_reasoning_effort(config.reasoning_effort);
        client.set_max_messages(config.max_messages);
        client.set_n_ctx(config.n_ctx);
        if config.encryption_enabled {
            if let Some(key) = config.encryption_key() {
                client.set_encryption_key(Some(key));
            }
        }
        let name = client
            .load_session()
            .map(|s| s.name)
            .unwrap_or(name);

        // Route tool-call events into the shared channel.
        client.set_tool_event_sender(event_tx.clone());

        // Per-session engine: bound to this session's client so the agent
        // chat loop reads/writes an isolated conversation store (the shared
        // bootstrap engine's client would otherwise be mutated by every
        // session in parallel — a cross-session data race).
        let engine = template_engine
            .clone()
            .with_client(client.clone())
            .with_session_id(session_id.clone());
        let pipeline = ChatPipeline::new(
            Arc::new(engine.clone()),
            event_tx,
            config.reasoning_effort,
            session_id.clone(),
        );

        Self {
            session_id,
            name,
            client,
            pipeline,
            engine,
            cancel_token: CancellationToken::new(),
            selected_agent: None,
            chat_state: ChatAreaState::default(),
        }
    }

    /// Cancel the current task in this session.
    pub fn cancel(&self) {
        self.cancel_token.cancel();
        self.pipeline.cancel();
    }

    /// Check if there's an active task running.
    pub fn is_generating(&self) -> bool {
        self.chat_state.is_generating
    }

    /// Refresh this session's token gauge using the exact char counter shared
    /// with the trim logic — the gauge always reflects what the trimmer sees.
    /// Sets `token_count` (approximate tokens via the client's calibrated
    /// chars-per-token ratio) and `context_used` (percent of the effective
    /// n_ctx budget). Used as a fallback for backends that omit per-round
    /// usage stats.
    pub fn refresh_token_gauge(&mut self, n_ctx: u32) {
        let chars = crate::client::estimate_conversation_tokens(self.client.conversation());
        self.chat_state.token_count = self.client.estimate_tokens_from_chars(chars);
        if n_ctx > 0 {
            self.chat_state.context_used =
                self.chat_state.token_count as f32 / n_ctx as f32 * 100.0;
        }
    }
}
