use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::types::{AppEvent, ChatToolPolicy, QueuedMessage, ReasoningMode};

use super::{AgentEngine, RunParams};

/// Active-run state for the pipeline: the run's cancel token, the running
/// task's handle (for abort and finish polling), and the shared receiver of
/// the run's injection channel.
///
/// Everything is swapped under one `Mutex` with short critical sections
/// (lock, clone, unlock — never held across an `.await`): token, handle and
/// channel ends are all `Send`, so no raw pointers or manual `Send`/`Sync`
/// impls are needed.
struct RunState {
    token: Option<CancellationToken>,
    handle: Option<JoinHandle<()>>,
    injection: Option<Arc<Mutex<mpsc::Receiver<QueuedMessage>>>>,
    /// Run-scoped per-call tool-cancellation registry: every in-flight tool
    /// call registers its own token here keyed `"{session_id}:{call_id}"`,
    /// so the UI can stop ONE live tool card (see `cancel_tool`).
    tool_cancel: Option<Arc<crate::tools::cancel::CancelRegistry>>,
}

/// ChatPipeline routes chat requests through the AgentEngine's tool pipeline,
/// using the selected agent's system prompt instead of routing through the
/// agent registry.
///
/// This is the chat path equivalent of /plan — same native tool-calling loop,
/// same reasoning-content tracking, same cancellation model.
pub struct ChatPipeline {
    agent_engine: Arc<AgentEngine>,
    /// The current run's state (cancel token, task handle, injection
    /// receiver), swapped at `start()`/`cancel()` time.
    run: Mutex<RunState>,
    /// Event sender for forwarding pipeline events to the UI.
    event_tx: mpsc::Sender<AppEvent>,
    /// Reasoning-effort selection for this session's runs: Auto (default)
    /// follows the selected agent profile's own `reasoning_effort`;
    /// Explicit(e) forces level e for every run.
    reasoning_mode: ReasoningMode,
    /// Session ID for routing events.
    session_id: String,
    /// Sender half of the mid-run injection channel. `inject()` (UI thread)
    /// hands user messages sent while this run is active to the running agent
    /// loop, which picks them up at the next LLM round boundary.
    injection_tx: Mutex<mpsc::Sender<QueuedMessage>>,
}

impl ChatPipeline {
    pub fn new(
        agent_engine: Arc<AgentEngine>,
        event_tx: mpsc::Sender<AppEvent>,
        reasoning_mode: ReasoningMode,
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
            run: Mutex::new(RunState {
                token: None,
                handle: None,
                injection: None,
                tool_cancel: None,
            }),
            event_tx,
            reasoning_mode,
            session_id,
            injection_tx: Mutex::new(injection_tx),
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

        // Create a fresh cancel token and injection channel for this run:
        // the UI can `inject()` user messages into it any time, and the
        // running task wires the receiver into the agent (drained at LLM
        // round boundaries). The shared clone also serves the cancel-time
        // remainder drain in `cancel()`. The task gets its own clone of the
        // token; the stored one is what `cancel()` signals.
        let task_token = CancellationToken::new();
        let (injection_tx, injection_rx) = mpsc::channel();
        let injection_holder = Arc::new(Mutex::new(injection_rx));
        // Run-scoped per-call tool-cancellation registry: the agent loop
        // registers every in-flight tool call here (keyed
        // "{session_id}:{call_id}"), and `cancel_tool` addresses one call by
        // key (the live tool card's Stop button).
        let tool_cancel = Arc::new(crate::tools::cancel::CancelRegistry::new());
        let mut run = self.run.lock().unwrap();
        run.token = Some(task_token.clone());
        run.injection = Some(Arc::clone(&injection_holder));
        run.tool_cancel = Some(Arc::clone(&tool_cancel));
        drop(run);
        *self.injection_tx.lock().unwrap() = injection_tx;

        let agent_engine = self.agent_engine.clone();
        let event_tx = self.event_tx.clone();
        let reasoning_mode = self.reasoning_mode;
        let session_id = self.session_id.clone();
        let prompt = prompt.to_string();
        let system_prompt = system_prompt.to_string();
        let tool_policy = tool_policy.clone();
        let image = image.map(str::to_string);
        let handle = tokio::spawn(async move {
            // Run-scoped values ride on `RunParams` — the shared session
            // engine serves this run directly (no per-run engine clone, no
            // re-Arc): chain events reach the UI via `event_tx`, the
            // session's reasoning-effort mode (Auto = agent profile's own
            // effort; Explicit = forced level) is applied per run, and the
            // mid-run injection channel (user messages sent while this run is
            // active) is injected at the next LLM round boundary.
            let params = RunParams {
                event_tx: Some(Arc::new(Mutex::new(event_tx.clone()))),
                session_id: Some(session_id.clone()),
                reasoning: reasoning_mode,
                injection: Some(injection_holder),
                tool_cancel: Some(tool_cancel),
            };

            tracing::info!("[CHAT PIPELINE] Starting chat with prompt: {}", prompt);

            let result = tokio::select! {
                result = agent_engine.execute_with_tools(&prompt, &system_prompt, &tool_policy, image.as_deref(), &task_token, &params) => result,
                _ = task_token.cancelled() => {
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

        self.run.lock().unwrap().handle = Some(handle);
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
        match self.run.lock().unwrap().handle.as_ref() {
            Some(handle) => handle.is_finished(),
            None => true,
        }
    }

    /// Stop the current chat session.
    pub fn cancel(&self) {
        // Cancel the active token, and drain any user messages that were
        // injected but not yet consumed (the task is about to be aborted and
        // would drop them): hand them back to the UI so they run as the next
        // turn instead of being lost.
        let (token, injection) = {
            let run = self.run.lock().unwrap();
            (run.token.clone(), run.injection.clone())
        };
        if let Some(token) = token {
            token.cancel();
        }
        if let Some(holder) = injection {
            let rx = holder.lock().unwrap();
            while let Ok(message) = rx.try_recv() {
                let _ = self.event_tx.send(AppEvent::UserMessageDrained {
                    message: Box::new(message),
                    session_id: self.session_id.clone(),
                });
            }
        }
        if let Some(registry) = {
            self.run.lock().unwrap().tool_cancel.clone()
        } {
            // Defensive: the run token cascade already cancels every
            // per-call token (they are children of it).
            registry.cancel_all();
        }
        // Clear this run's injection channel and abort the running task if any.
        let mut run = self.run.lock().unwrap();
        run.injection = None;
        run.tool_cancel = None;
        if let Some(handle) = run.handle.take() {
            handle.abort();
        }
    }

    /// Stop ONE in-flight tool call (the live tool card's Stop button).
    ///
    /// `key` is `"{session_id}:{call_id}"` — the same key the agent loop
    /// registered the call under. Returns `false` when no in-flight call
    /// carries that key (already finished, or no run is active).
    ///
    /// A cancelled tool call yields a tool-result error
    /// ("Error: …cancelled…") that the agent loop feeds back to the model,
    /// so the run CONTINUES (unlike [`Self::cancel`], which stops the whole
    /// run).
    pub fn cancel_tool(&self, key: &str) -> bool {
        match self.run.lock().unwrap().tool_cancel.as_ref() {
            Some(registry) => registry.cancel(key),
            None => false,
        }
    }
}
