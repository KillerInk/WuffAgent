use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::sync::atomic::{AtomicPtr, Ordering};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agents::config::ShellConfig;
use crate::agents::AgentEngine;
use crate::types::{AppEvent, ReasoningEffort};

/// The selected chat profile's tool policy, carried onto the chat path so the
/// chat agent runs with that profile's tool set, shell restrictions, handoff
/// rights, reasoning effort, and trim configuration (instead of the old
/// "all tools + allow-all shell" default).
#[derive(Clone)]
pub struct ChatToolPolicy {
    /// Tool names the profile authorizes. The chat agent is given all available
    /// tools plus `shell`, so an empty list does not strip tools from chat.
    pub allowed_tools: Vec<String>,
    /// The profile's shell config (allowlist/enabled/timeout).
    pub shell_config: ShellConfig,
    /// The selected profile's name (used for the chat agent's identity and
    /// handoff markers, e.g. "[Handoff from 'architect' to 'coder']").
    pub agent_name: String,
    /// Whether the chat agent may hand off the session via the `handoff` tool
    /// (the profile's `handoff_enabled` flag).
    pub handoff_enabled: bool,
    /// Agent names the profile may hand off to (empty = any enabled agent).
    pub handoff_targets: Vec<String>,
    /// The profile's reasoning effort (Off = inherit the global toggle).
    pub reasoning_effort: ReasoningEffort,
    /// The profile's context-trimming configuration.
    pub trim_config: crate::trimming::config::TrimConfig,
}

impl ChatToolPolicy {
    /// A permissive policy: all tools + an unrestricted (allow-all) shell.
    /// Used when no profile is selected or none is found.
    pub fn unrestricted() -> Self {
        Self {
            allowed_tools: Vec::new(),
            shell_config: ShellConfig {
                shell_enabled: true,
                allowed_commands: Vec::new(),
                ..ShellConfig::default()
            },
            agent_name: "chat".to_string(),
            // Permissive = may hand off to any enabled agent (empty targets).
            // Without this, "Auto" chat could never hand off to a profile,
            // even though every other tool is unrestricted.
            handoff_enabled: true,
            handoff_targets: Vec::new(),
            reasoning_effort: ReasoningEffort::default(),
            trim_config: crate::trimming::config::TrimConfig::default(),
        }
    }
}

/// ChatPipeline routes chat requests through the AgentEngine's tool pipeline,
/// using the selected agent's system prompt instead of routing through the
/// agent registry.
///
/// This is the chat path equivalent of /plan — same native tool-calling loop,
/// same reasoning-content tracking, same cancellation model.
pub struct ChatPipeline {
    agent_engine: Arc<AgentEngine>,
    /// The active cancel token for the current request (or the last one if
    /// the task has already finished). Stored as raw ptr so we can mutate
    /// without interior mutability on the token itself.
    current_token: AtomicPtr<CancellationToken>,
    /// Handle to the running task (for abort).
    task_handle: Mutex<Option<JoinHandle<()>>>,
    /// Event sender for forwarding pipeline events to the UI.
    event_tx: mpsc::Sender<AppEvent>,
    /// Reasoning effort level.
    reasoning_effort: ReasoningEffort,
    /// Session ID for routing events.
    session_id: String,
}

// Safe: only AtomicPtr+CancellationToken is used, no actual concurrency.
unsafe impl Send for ChatPipeline {}
unsafe impl Sync for ChatPipeline {}

impl ChatPipeline {
    pub fn new(
        agent_engine: Arc<AgentEngine>,
        event_tx: mpsc::Sender<AppEvent>,
        reasoning_effort: ReasoningEffort,
        session_id: String,
    ) -> Self {
        Self {
            agent_engine,
            current_token: AtomicPtr::new(Box::into_raw(Box::new(CancellationToken::new()))),
            task_handle: Mutex::new(None),
            event_tx,
            reasoning_effort,
            session_id,
        }
    }

    /// Start a chat session with the given prompt, system prompt, and tool policy.
    ///
    /// `image` is an optional `data:` URI (e.g. `data:image/png;base64,...`)
    /// for a user-attached image; it is recorded on the user message so the
    /// model receives it (and it persists with the session).
    pub fn start(&self, prompt: &str, system_prompt: &str, tool_policy: &ChatToolPolicy, image: Option<&str>) {
        // Cancel any existing task
        self.cancel();

        // Create a fresh cancel token for this request
        let cancel_token = CancellationToken::new();
        let new_ptr = Box::into_raw(Box::new(cancel_token));
        // Swap in the new token. The old one is no longer referenced by any
        // live task (we just aborted it above and `cancel()` already signalled
        // it), so free it now — otherwise each `start()` leaks a boxed
        // CancellationToken.
        let old_ptr = self.current_token.swap(new_ptr, Ordering::AcqRel);
        if !old_ptr.is_null() {
            unsafe { drop(Box::from_raw(old_ptr)) };
        }

        let agent_engine = self.agent_engine.clone();
        let cancel_token = unsafe { &*new_ptr };
        let event_tx = self.event_tx.clone();
        let reasoning_effort = self.reasoning_effort;
        let session_id = self.session_id.clone();
        let prompt = prompt.to_string();
        let system_prompt = system_prompt.to_string();
        let tool_policy = tool_policy.clone();
        let image = image.map(str::to_string);
        let handle = tokio::spawn(async move {
            // Wire the event tx into the engine so chain events reach the UI,
            // and apply the current reasoning effort setting.
            let inner_engine = (*agent_engine).clone();
            let engine = Arc::new(
                inner_engine
                    .with_event_tx(Arc::new(Mutex::new(event_tx.clone())))
                    .with_reasoning_effort(reasoning_effort)
                    .with_session_id(session_id.clone()),
            );

            tracing::info!("[CHAT PIPELINE] Starting chat with prompt: {}", prompt);

            let result = tokio::select! {
                result = engine.execute_with_tools(&prompt, &system_prompt, &tool_policy, image.as_deref(), cancel_token) => result,
                _ = cancel_token.cancelled() => {
                    Ok(String::from("[CANCELLED]"))
                }
            };

            // On success the agent loop already emitted StreamComplete (with
            // usage), so we only surface failures here — re-sending
            // StreamComplete makes the UI append the response a second time.
            //
            // A cancel/abort of this task (new `start()`, session drop, stop
            // button) is handled by the UI's `task_done()` sweep, which clears
            // the generating state when the task is finished — so we do NOT
            // emit a terminal event from the cancel branch (that would race
            // with the sweep and could double-fire).
            if let Err(e) = result {
                tracing::error!("[CHAT PIPELINE] Failed: {}", e);
                let _ = event_tx.send(AppEvent::StreamError { error: e.to_string(), session_id });
            }
        });

        *self.task_handle.lock().unwrap() = Some(handle);
    }

    /// Returns true when the pipeline has no task, or the current task has
    /// finished (completed, failed, or was aborted).
    ///
    /// The UI polls this on every frame as a defensive sweep: if
    /// `is_generating` is still set while the task is done, a terminal event
    /// was lost (e.g. the task was aborted by a new `start()` before it could
    /// emit `StreamComplete`/`StreamError`), and the UI clears its generating
    /// state itself so the spinner cannot get stuck.
    pub fn task_done(&self) -> bool {
        match self.task_handle.lock().unwrap().as_ref() {
            Some(handle) => handle.is_finished(),
            None => true,
        }
    }

    /// Stop the current chat session.
    pub fn cancel(&self) {
        // Cancel the active token
        let ptr = self.current_token.load(Ordering::Acquire);
        if !ptr.is_null() {
            unsafe { (*ptr).cancel() };
        }
        // Abort the running task if any
        if let Some(handle) = self.task_handle.lock().unwrap().take() {
            handle.abort();
        }
    }
}

impl Drop for ChatPipeline {
    fn drop(&mut self) {
        // Drop the current token
        let ptr = self.current_token.load(Ordering::Acquire);
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}
