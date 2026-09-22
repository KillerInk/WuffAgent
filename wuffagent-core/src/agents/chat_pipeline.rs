use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::types::{AppEvent, ChatToolPolicy, QueuedMessage, ReasoningEffort};

use super::AgentEngine;

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
    /// Sender half of the mid-run injection channel. `inject()` (UI thread)
    /// hands user messages sent while this run is active to the running agent
    /// loop, which picks them up at the next LLM round boundary.
    injection_tx: Mutex<mpsc::Sender<QueuedMessage>>,
    /// Receiver half of the CURRENT run's injection channel, shared with the
    /// running task (the agent loop drains it at round boundaries; the engine
    /// drains the remainder after the loop ends). `cancel()` drains it before
    /// aborting the task so a late message is never lost. `None` between runs.
    /// (The outer `Mutex` is touched only from the UI thread; the inner
    /// `Mutex` serializes the task-side and cancel-side drains, so each
    /// message is consumed exactly once.)
    injection_rx: Mutex<Option<Arc<Mutex<mpsc::Receiver<QueuedMessage>>>>>,
}

// Safe: AtomicPtr+CancellationToken + std mpsc ends (all `Send`/`Sync`
// through the `Mutex`es); the raw ptr is only swapped from the UI thread.
unsafe impl Send for ChatPipeline {}
unsafe impl Sync for ChatPipeline {}

impl ChatPipeline {
    pub fn new(
        agent_engine: Arc<AgentEngine>,
        event_tx: mpsc::Sender<AppEvent>,
        reasoning_effort: ReasoningEffort,
        session_id: String,
    ) -> Self {
        // Pre-start channel: its receiver is dropped immediately so an
        // `inject()` before `start()` simply fails (nothing is running yet —
        // the UI then falls back to the per-session queue). `start()` swaps in
        // a fresh channel for each run.
        let (injection_tx, _injection_rx) = mpsc::channel();
        drop(_injection_rx);
        Self {
            agent_engine,
            current_token: AtomicPtr::new(Box::into_raw(Box::new(CancellationToken::new()))),
            task_handle: Mutex::new(None),
            event_tx,
            reasoning_effort,
            session_id,
            injection_tx: Mutex::new(injection_tx),
            injection_rx: Mutex::new(None),
        }
    }

    /// Hand a user message (sent while this run is active) to the running
    /// agent loop for immediate injection.
    ///
    /// Returns `true` when the message was accepted: the agent loop picks it
    /// up at the next LLM round boundary (the earliest point the model can
    /// see it) and appends it to the current turn; if the run ends before
    /// that, the engine hands it back to the UI to run as the next turn.
    /// Returns `false` when no run is live (the UI falls back to the
    /// per-session `queued_messages` queue).
    pub fn inject(&self, message: QueuedMessage) -> bool {
        self.injection_tx.lock().unwrap().send(message).is_ok()
    }

    /// Start a chat session with the given prompt, system prompt, and tool policy.
    ///
    /// `image` is an optional `data:` URI (e.g. `data:image/png;base64,...`)
    /// for a user-attached image; it is recorded on the user message so the
    /// model receives it (and it persists with the session).
    pub fn start(
        &self,
        prompt: &str,
        system_prompt: &str,
        tool_policy: &ChatToolPolicy,
        image: Option<&str>,
    ) {
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
        // Fresh injection channel for this run: the UI can `inject()` user
        // messages into it any time, and the running task wires the receiver
        // into the agent (drained at LLM round boundaries). The shared clone
        // also serves the cancel-time remainder drain in `cancel()`.
        let (injection_tx, injection_rx) = mpsc::channel();
        *self.injection_tx.lock().unwrap() = injection_tx;
        let injection_holder = Arc::new(Mutex::new(injection_rx));
        *self.injection_rx.lock().unwrap() = Some(Arc::clone(&injection_holder));
        let handle = tokio::spawn(async move {
            // Wire the event tx into the engine so chain events reach the UI,
            // apply the current reasoning effort setting, and attach the
            // mid-run injection channel (user messages sent while this run is
            // active are injected at the next LLM round boundary).
            let inner_engine = (*agent_engine).clone();
            let engine = Arc::new(
                inner_engine
                    .with_event_tx(Arc::new(Mutex::new(event_tx.clone())))
                    .with_reasoning_effort(reasoning_effort)
                    .with_session_id(session_id.clone())
                    .with_injection_channel(Some(injection_holder)),
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
                let _ = event_tx.send(AppEvent::StreamError {
                    error: e.to_string(),
                    session_id,
                });
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
        // Drain any user messages that were injected but not yet consumed
        // (the task is about to be aborted and would drop them): hand them
        // back to the UI so they run as the next turn instead of being lost.
        if let Some(holder) = self.injection_rx.lock().unwrap().as_ref() {
            let rx = holder.lock().unwrap();
            while let Ok(message) = rx.try_recv() {
                let _ = self.event_tx.send(AppEvent::UserMessageDrained {
                    message: Box::new(message),
                    session_id: self.session_id.clone(),
                });
            }
        }
        *self.injection_rx.lock().unwrap() = None;
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
