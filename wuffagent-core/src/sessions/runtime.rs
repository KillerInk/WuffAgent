use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::agents::AgentEngine;
use crate::client::ChatPipeline;

/// A message sent while the AI is still working. Displayed in the chat
/// immediately and — via the pipeline's injection channel — handed to the
/// RUNNING agent loop, which injects it into the current turn at the next LLM
/// round boundary (the earliest point the model can see it), instead of
/// waiting for the whole run to finish.
///
/// The `queued_messages` fallback queue (see [`ChatAreaState::queued_messages`])
/// holds a `QueuedMessage` only when injection was not possible (the run
/// already finished or was cancelled before delivery, or the message is
/// re-delivered while a new run is already in flight); those are processed as
/// the next turn once the current run (and any earlier queued messages)
/// finishes.
#[derive(Clone, Debug)]
pub struct QueuedMessage {
    pub text: String,
    pub image: Option<egui::ImageSource<'static>>,
    /// System prompt resolved from the selected agent at send time.
    pub agent_prompt: String,
    /// Tool policy (allowed_tools + shell config) resolved from the selected agent.
    pub tool_policy: crate::client::pipeline::ChatToolPolicy,
}

/// A tool call currently executing, shown as a live card at the end of the
/// transcript (spinner + args + live output tail) until its completion event
/// turns it into a persisted `MessageKind::Tool` message.
#[derive(Clone)]
pub struct ActiveTool {
    pub tool_name: String,
    pub call_id: String,
    /// One-line preview of the call's arguments ("what is it doing").
    pub args_preview: String,
    /// When the call started (drives the live elapsed-time readout).
    pub started_at: std::time::Instant,
    /// Latest tail of live output reported by the tool (e.g. shell output
    /// lines). Replaced, not appended, on every progress event.
    pub live_output: String,
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
    /// S2: the message index whose 👎 feedback comment field is open (None = closed).
    pub feedback_comment_for: Option<usize>,
    /// S2: the comment being typed for the open feedback field.
    pub feedback_comment: String,
    /// S2: ratings already recorded in this session (display index -> "good"/"bad").
    /// Index-aligned with `messages` (shifted on delete); used to highlight the
    /// chosen button and prevent double-saving a rating.
    pub message_ratings: std::collections::HashMap<usize, String>,
    pub context_used: f32,
    pub token_count: usize,
    /// llama.cpp server-reported speeds from the last completed round:
    /// prompt processing (tokens/s). `None` until a backend that reports
    /// timings has completed a round (or cleared by a backend that doesn't).
    pub prompt_tps: Option<f64>,
    /// Token generation (tokens/s). While a round is generating this is the
    /// LIVE estimate of the in-progress segment; on round/complete it snaps
    /// to the llama.cpp "predicted" speed from the server's `timings`
    /// (or None if the backend doesn't report them).
    pub gen_tps: Option<f64>,
    /// Content characters streamed in the current (in-progress) generation
    /// segment — the status bar's live token/speed estimate. Reset by
    /// `commit_stream`.
    pub live_gen_chars: u32,
    /// Latest llama.cpp `prompt_progress` for the in-flight round (the
    /// prompt is being processed — no tokens have streamed yet). Cleared
    /// when the round completes or errors.
    pub prompt_progress: Option<crate::types::PromptProgress>,
    /// When the current generation segment started (live speed estimate).
    pub live_gen_started: Option<std::time::Instant>,
    /// Estimated tokens of the current segment ALREADY added to `token_count`
    /// by `update_live_estimates` (content + thinking chunks). `live_gen_tokens()`
    /// returns the segment CUMULATIVE, so without this marker every chunk would
    /// re-add the whole segment and the live gauge would inflate quadratically.
    /// Reset by `commit_stream`.
    pub live_tokens_added: f64,
    pub pending_image: Option<egui::ImageSource<'static>>,
    /// Status shown in the status bar for this session.
    pub status: crate::types::AppStatus,
    /// Fallback queue for messages that could not be injected into the
    /// running agent loop (the run finished/cancelled before delivery, or the
    /// message was re-delivered while a new run was already in flight).
    /// Drained as the next turn when the current run ends.
    pub queued_messages: Vec<QueuedMessage>,
    /// Tool calls currently executing (live cards). Populated on
    /// `ToolCallStart`, updated by `ToolCallProgress`, drained on
    /// `ToolCallComplete`/`ToolCallError` (or when the run ends).
    pub active_tools: Vec<ActiveTool>,
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
            feedback_comment_for: None,
            feedback_comment: String::new(),
            message_ratings: std::collections::HashMap::new(),
            context_used: 0.0,
            token_count: 0,
            prompt_tps: None,
            gen_tps: None,
            live_gen_chars: 0,
            live_gen_started: None,
            live_tokens_added: 0.0,
            prompt_progress: None,
            pending_image: None,
            status: crate::types::AppStatus::Stopped,
            queued_messages: Vec::new(),
            active_tools: Vec::new(),
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
        self.bump_live_gen(chunk);
    }

    /// Accumulate a thinking chunk: live display buffer plus the SAME live
    /// token/speed counters as `stream_chunk`. Thinking tokens are generated
    /// tokens too (they consume context), so the TG speed pill and the live
    /// token gauge must track them — before this, TG froze (or never appeared
    /// when thinking led a round) for the whole duration of thinking segments.
    pub fn stream_thinking_chunk(&mut self, chunk: &str) {
        self.current_thinking.push_str(chunk);
        self.bump_live_gen(chunk);
    }

    /// Bump the live generation estimate (chars + segment start time) with a
    /// chunk of streamed output (content or thinking).
    fn bump_live_gen(&mut self, chunk: &str) {
        self.live_gen_chars = self
            .live_gen_chars
            .saturating_add(chunk.chars().count() as u32);
        if self.live_gen_started.is_none() {
            self.live_gen_started = Some(std::time::Instant::now());
        }
    }

    pub fn commit_stream(&mut self) {
        let buffer = std::mem::take(&mut self.stream_buffer);
        if !buffer.is_empty() {
            self.append_message("assistant", &buffer);
        }
        // The generation segment ended (round commit, completion, error or
        // cancellation) — clear the live estimate so the next round starts
        // from a clean slate.
        self.live_gen_chars = 0;
        self.live_gen_started = None;
        self.live_tokens_added = 0.0;
    }

    /// Estimated tokens generated so far in the current segment
    /// (~3.5 characters per token).
    pub fn live_gen_tokens(&self) -> f64 {
        self.live_gen_chars as f64 / 3.5
    }

    /// Live token-generation speed (tokens/s) for the current segment,
    /// once enough time has elapsed to be meaningful.
    pub fn live_gen_tps(&self) -> Option<f64> {
        let started = self.live_gen_started?;
        let elapsed = started.elapsed().as_secs_f64();
        (elapsed >= 0.5).then(|| self.live_gen_tokens() / elapsed)
    }

    /// Update the status bar's live estimates (token gauge + TG speed) from
    /// the streamed output so far in the current segment — content AND
    /// thinking chunks feed `live_gen_chars`, so TG tracks both. Only the
    /// not-yet-counted delta is added to `token_count` (idempotent between
    /// chunks), and `gen_tps` takes the live segment speed once available.
    /// No-op while not generating; values snap to server-reported totals on
    /// round/complete.
    pub fn update_live_estimates(&mut self, n_ctx: u32) {
        if !self.is_generating {
            return;
        }
        let live = self.live_gen_tokens();
        let delta = live - self.live_tokens_added;
        if delta > 0.0 {
            self.token_count = (self.token_count as f64 + delta) as usize;
            self.live_tokens_added = live;
            if n_ctx > 0 {
                self.context_used =
                    (self.token_count as f64 / n_ctx as f64 * 100.0) as f32;
            }
        }
        if let Some(tps) = self.live_gen_tps() {
            self.gen_tps = Some(tps);
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
            // The model layer stores the image as a `data:` URI; the chat
            // display wants raw base64 (see `ChatMessage.image`).
            let image = msg
                .image
                .as_ref()
                .and_then(|uri| uri.rsplit_once("base64,"))
                .map(|(_, b64)| b64.to_string());
            self.messages.push(crate::types::ChatMessage {
                kind,
                role: msg.role.clone(),
                content: msg.content.clone(),
                timestamp: ts,
                image,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_chunks_feed_live_estimate() {
        let mut s = ChatAreaState::default();
        s.is_generating = true;
        // Thinking chunk: display buffer + live counters (TG must track it).
        s.stream_thinking_chunk("hello world"); // 11 chars
        assert_eq!(s.current_thinking, "hello world");
        assert_eq!(s.live_gen_chars, 11);
        assert!(s.live_gen_started.is_some());
        // Content chunk: same counters, different buffers.
        s.stream_chunk("abc"); // 3 chars
        assert_eq!(s.stream_buffer, "abc");
        assert_eq!(s.current_thinking, "hello world");
        assert_eq!(s.live_gen_chars, 14);
        assert!((s.live_gen_tokens() - 14.0 / 3.5).abs() < 1e-9);
    }

    #[test]
    fn update_live_estimates_no_double_count() {
        let mut s = ChatAreaState::default();
        s.is_generating = true;
        s.token_count = 100;
        s.stream_chunk(&"x".repeat(35)); // ~10 estimated tokens
        s.update_live_estimates(1000);
        let after_first = s.token_count;
        assert!(after_first > 100, "live gauge must grow while generating");
        // Applying again WITHOUT new chunks must add nothing (the old code
        // re-added the whole cumulative segment every chunk — quadratic).
        s.update_live_estimates(1000);
        assert_eq!(s.token_count, after_first);
        // A new chunk adds only its own share.
        s.stream_chunk(&"y".repeat(35));
        s.update_live_estimates(1000);
        let after_second = s.token_count;
        assert!(after_second > after_first);
        assert!((after_second as f64 - after_first as f64 - 35.0 / 3.5).abs() < 1.0);
    }

    #[test]
    fn update_live_estimates_noop_when_not_generating() {
        let mut s = ChatAreaState::default();
        s.token_count = 42;
        s.stream_chunk(&"x".repeat(35));
        s.update_live_estimates(1000);
        assert_eq!(s.token_count, 42);
        assert!(s.gen_tps.is_none());
        assert_eq!(s.context_used, 0.0);
    }

    #[test]
    fn update_live_estimates_sets_context_used() {
        let mut s = ChatAreaState::default();
        s.is_generating = true;
        s.stream_chunk(&"x".repeat(350)); // 100 estimated tokens
        s.update_live_estimates(1000);
        assert!((s.context_used - (100.0 / 1000.0 * 100.0)).abs() < 1.0);
    }

    #[test]
    fn commit_stream_resets_live_estimate() {
        let mut s = ChatAreaState::default();
        s.is_generating = true;
        s.token_count = 100;
        s.stream_chunk(&"x".repeat(35));
        s.update_live_estimates(1000);
        let grown = s.token_count;
        assert!(grown > 100);
        s.commit_stream();
        assert_eq!(s.live_gen_chars, 0);
        assert!(s.live_gen_started.is_none());
        assert_eq!(s.live_tokens_added, 0.0);
        // Next segment starts counting from a clean slate.
        s.stream_chunk(&"y".repeat(35));
        s.update_live_estimates(1000);
        assert!(s.token_count > grown);
    }

    #[test]
    fn live_gen_tps_0_5s_gate() {
        let mut s = ChatAreaState::default();
        s.stream_chunk("some text");
        assert!(s.live_gen_tps().is_none(), "under 0.5s the estimate is meaningless");
        std::thread::sleep(std::time::Duration::from_millis(550));
        let tps = s.live_gen_tps().expect("after 0.5s the live speed is available");
        assert!(tps > 0.0);
    }
}
