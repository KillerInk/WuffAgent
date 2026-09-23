//! Parallel tool execution for `run_llm_loop` (extracted A1).
//!
//! While the model is still streaming (often: still reasoning), a tool call
//! becomes executable the moment the stream moves past it. The ready callback
//! returned by `PendingToolRuns::ready` spawns its execution in the
//! background at that point, so tools run while the model keeps thinking.
//! Results are collected in call order after the stream ends (see the
//! native tool-call section in `tool_calls.rs`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::tools::ToolManager;
use crate::types::{AppEvent, ToolCall};

/// UI event channel for one run, so `'static` streaming closures can capture
/// a cheap `Arc` clone and still send events.
#[derive(Clone)]
pub(crate) struct EventSink {
    tx: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    session_id: String,
}

impl EventSink {
    pub(crate) fn new(
        tx: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
        session_id: String,
    ) -> Self {
        Self { tx, session_id }
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn send(&self, event: AppEvent) {
        if let Some(tx) = &self.tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }
}

/// Build the live-progress sink for a running tool call. Reports arrive as
/// `ToolCallProgress` events (latest-tail semantics) which the UI renders in
/// the live tool card. Returns a no-op sink when no UI channel is attached
/// (tests, headless runs).
pub(crate) fn tool_progress_for(
    tx: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    session_id: &str,
    tool_name: &str,
    call_id: &str,
) -> crate::tools::types::ToolProgress {
    match tx {
        Some(tx) => {
            let tx = Arc::clone(&tx);
            let name = tool_name.to_string();
            let id = call_id.to_string();
            let sid = session_id.to_string();
            crate::tools::types::ToolProgress {
                on_progress: Some(Arc::new(move |text: &str| {
                    if let Ok(g) = tx.lock() {
                        let _ = g.send(AppEvent::ToolCallProgress {
                            tool_name: name.clone(),
                            call_id: id.clone(),
                            text: text.to_string(),
                            session_id: sid.clone(),
                        });
                    }
                })),
            }
        }
        None => crate::tools::types::ToolProgress::none(),
    }
}

/// Early-start run map: call id → background execution handle
/// (Ok = result text, Err = argument-parse error).
type PendingRunMap = Arc<
    Mutex<HashMap<String, tokio::task::JoinHandle<Result<String, String>>>>,
>;

/// Tool runs that were early-started while the model was still streaming.
///
/// Owns the `id → JoinHandle` map plus everything the ready callback needs
/// (event channel, tool manager, cancel token) so the `'static` closure
/// construction lives here instead of in `run_llm_loop`.
pub(crate) struct PendingToolRuns {
    map: PendingRunMap,
    sink: EventSink,
    manager: Arc<Mutex<ToolManager>>,
    cancel: CancellationToken,
}

impl PendingToolRuns {
    pub(crate) fn new(
        sink: EventSink,
        manager: ToolManager,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
            sink,
            manager: Arc::new(Mutex::new(manager)),
            cancel,
        }
    }

    /// Abort every early-started run that has not been collected yet — used
    /// when the turn ends (cancel, error, overflow-retry) so nothing keeps
    /// executing in the background for a dead turn.
    pub(crate) fn abort_all(&self) {
        for h in self.map.lock().unwrap().values() {
            h.abort();
        }
    }

    /// Drop all handles (overflow-retry: the request is re-issued fresh).
    pub(crate) fn clear(&self) {
        self.map.lock().unwrap().clear();
    }

    /// Take the early-started handle for `call_id` (if any) out of the map.
    pub(crate) fn take(
        &self,
        call_id: &str,
    ) -> Option<tokio::task::JoinHandle<Result<String, String>>> {
        self.map.lock().unwrap().remove(call_id)
    }

    /// The `'static` ready callback for `ChatClient::stream_with_messages_arc`.
    ///
    /// Early-starts a tool call while the model is still streaming: the SSE
    /// layer only reports a call once its arguments are complete, so it is
    /// safe to execute now.
    pub(crate) fn ready(self: &Arc<Self>) -> impl FnMut(ToolCall) + Send + Sync + 'static {
        let runs = Arc::clone(self);
        move |call: ToolCall| {
            let id = call.id.clone();
            if id.is_empty() {
                return;
            }
            if runs.map.lock().unwrap().contains_key(&id) {
                return; // defensive: already started
            }
            let name = call.function.name.clone();
            let args = call.function.arguments.clone();
            tracing::debug!(
                "[AGENT] Early-starting tool '{}' (id={}) while model is still streaming",
                name, id
            );
            // Live tool card: args preview + progress sink.
            let args_preview = crate::tools::tool_args_summary(&name, &args);
            runs.sink.send(AppEvent::ToolCallStart {
                tool_name: name.clone(),
                call_id: id.clone(),
                args_preview,
                session_id: runs.sink.session_id().to_string(),
            });
            let progress = tool_progress_for(
                runs.sink.tx.clone(),
                runs.sink.session_id(),
                &name,
                &id,
            );
            let tool_mgr = runs.manager.clone();
            let token = runs.cancel.clone();
            let handle = tokio::spawn(async move {
                let params = match crate::tools::manager::parse_tool_args(&args) {
                    Ok(p) => p,
                    Err(e) => return Err(e),
                };
                // Clone the manager (cheap Arc clones inside) so no mutex
                // guard lives across the awaits in the select! below.
                let mgr = tool_mgr.lock().unwrap().clone();
                let result = tokio::select! {
                    r = mgr.execute_with_progress(&name, params, &progress) => r,
                    _ = token.cancelled() => {
                        return Ok(format!(
                            "Error: Cancelled (tool '{}' aborted)",
                            name
                        ))
                    }
                };
                Ok(match result {
                    Ok(output) => {
                        let s = format!("{}", output);
                        if s.trim().is_empty() {
                            "(no output)".to_string()
                        } else {
                            s
                        }
                    }
                    Err(e) => format!("Error: {}", e),
                })
            });
            runs.map.lock().unwrap().insert(id, handle);
        }
    }
}
