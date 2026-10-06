//! Parallel tool execution for `run_llm_loop` (extracted A1).
//!
//! Every tool call runs in the background:
//! - while the model is still streaming (often: still reasoning), a call
//!   becomes executable the moment the stream moves past it — the ready
//!   callback returned by `PendingToolRuns::ready` starts it then, so tools
//!   run while the model keeps thinking;
//! - at stream end, the collection in `tool_calls.rs` starts whatever the
//!   stream never moved past (typically the LAST call) plus all
//!   text-embedded fallback calls, in parallel.
//!
//! Results are always collected in CALL order (request list + shared store
//! must stay in lockstep); completions may arrive out of order (the UI keys
//! cards on call_id).

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

/// Tool runs started in the background: early-starts while the model is
/// still streaming, plus end-of-stream starts for the calls the stream never
/// "moved past" (typically the last one) and all text-embedded fallback
/// calls. Every execution path funnels through `start`/`execute_tool_call`,
/// so parsing, cancellation, and result normalization are identical.
pub(crate) struct PendingToolRuns {
    map: PendingRunMap,
    sink: EventSink,
    /// Cheap-clone manager (Arc fields inside), so each spawned task takes
    /// its own copy and no lock is held across awaits.
    manager: Arc<ToolManager>,
    /// The RUN-level cancel token: each tool call gets a child token of it
    /// (registered in `registry`) so a run-level cancel cascades to every
    /// in-flight call, while a per-call cancel (UI Stop button) stops only
    /// that one.
    cancel: CancellationToken,
    /// Per-call cancel tokens, keyed `"{session_id}:{call_id}"`.
    registry: Arc<crate::tools::cancel::CancelRegistry>,
}

/// Execute ONE tool call and normalize its result text.
///
/// Shared by the early-start path, the end-of-stream catch-up, and the
/// text-embedded fallback, so all of them agree on argument parsing,
/// cancellation semantics, and output normalization (empty → "(no output)",
/// tool error → "Error: …").
///
/// `Ok` = the normalized result text; `Err` = argument-parse error (the tool
/// never ran).
pub(crate) async fn execute_tool_call(
    manager: &ToolManager,
    cancel: &CancellationToken,
    name: &str,
    args: &str,
    progress: &crate::tools::types::ToolProgress,
) -> Result<String, String> {
    let params = match crate::tools::manager::parse_tool_args(args) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };
    let result = tokio::select! {
        // The token reaches the tool itself (the shell kills its process);
        // the second arm covers tools whose default impl ignores it.
        r = manager.execute_with_progress_and_cancel(name, params, progress, cancel) => r,
        _ = cancel.cancelled() => {
            return Ok(format!("Error: Cancelled (tool '{name}' aborted)"))
        }
    };
    Ok(match result {
        Ok(output) => {
            let s = format!("{output}");
            if s.trim().is_empty() {
                "(no output)".to_string()
            } else {
                s
            }
        }
        Err(e) => {
            tracing::warn!("[AGENT] Tool '{name}' failed: {e}");
            format!("Error: {e}")
        }
    })
}

impl PendingToolRuns {
    pub(crate) fn new(
        sink: EventSink,
        manager: ToolManager,
        cancel: CancellationToken,
        registry: Arc<crate::tools::cancel::CancelRegistry>,
    ) -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
            sink,
            manager: Arc::new(manager),
            cancel,
            registry,
        }
    }

    /// Abort every early-started run that has not been collected yet — used
    /// when the turn ends (cancel, error, overflow-retry) so nothing keeps
    /// executing in the background for a dead turn.
    pub(crate) fn abort_all(&self) {
        for h in self.map.lock().unwrap().values() {
            h.abort();
        }
        // Defensive: the run token cascade already cancels every per-call
        // token; cancel_all covers calls whose handles were taken out of the
        // map but are still running.
        self.registry.cancel_all();
    }

    /// Drop all handles (overflow-retry: the request is re-issued fresh).
    pub(crate) fn clear(&self) {
        self.map.lock().unwrap().clear();
        self.registry.cancel_all();
    }

    /// Take the early-started handle for `call_id` (if any) out of the map.
    pub(crate) fn take(
        &self,
        call_id: &str,
    ) -> Option<tokio::task::JoinHandle<Result<String, String>>> {
        self.map.lock().unwrap().remove(call_id)
    }

    /// Start a tool call in the background: emit its `ToolCallStart` event
    /// (the live UI card) and spawn its execution. No-op when the call was
    /// already started (defensive) or carries an empty id (the caller then
    /// executes it inline).
    ///
    /// Used by the stream ready callback (early-start) AND by the
    /// end-of-stream collection in `tool_calls.rs` (catch-up start for the
    /// calls the stream never moved past, and all text-embedded calls), so
    /// every path runs tools in parallel.
    pub(crate) fn start(&self, call: &ToolCall) {
        let id = call.id.clone();
        if id.is_empty() {
            return;
        }
        if self.map.lock().unwrap().contains_key(&id) {
            return; // defensive: already started
        }
        let name = call.function.name.clone();
        let args = call.function.arguments.clone();
        tracing::debug!(
            "[AGENT] Starting tool '{}' (id={}) in the background",
            name, id
        );
        // Live tool card: args preview + progress sink.
        let args_preview = crate::tools::tool_args_summary(&name, &args);
        self.sink.send(AppEvent::ToolCallStart {
            tool_name: name.clone(),
            call_id: id.clone(),
            args_preview,
            session_id: self.sink.session_id().to_string(),
        });
        let progress = tool_progress_for(
            self.sink.tx.clone(),
            self.sink.session_id(),
            &name,
            &id,
        );
        let tool_mgr = Arc::clone(&self.manager);
        // Per-call cancel token: keyed "{session}:{call_id}" so the UI can
        // address this exact live tool card (Stop button); child of the run
        // token so a run-level cancel cascades. Deregistered when the call
        // finishes.
        let key = format!("{}:{}", self.sink.session_id(), id);
        let token = self.registry.register(&key, Some(&self.cancel));
        let registry = Arc::clone(&self.registry);
        let key_owned = key;
        let handle = tokio::spawn(async move {
            let result = execute_tool_call(&tool_mgr, &token, &name, &args, &progress).await;
            registry.deregister(&key_owned);
            result
        });
        self.map.lock().unwrap().insert(id, handle);
    }

    /// The `'static` ready callback for `ChatClient::stream_with_messages_arc`.
    ///
    /// Early-starts a tool call while the model is still streaming: the SSE
    /// layer only reports a call once its arguments are complete, so it is
    /// safe to execute now.
    pub(crate) fn ready(self: &Arc<Self>) -> impl FnMut(ToolCall) + Send + Sync + 'static {
        let runs = Arc::clone(self);
        move |call: ToolCall| runs.start(&call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sleeps for `ms` (tool param, default 10) so tests can observe
    /// cancellation of a slow tool.
    struct SleepTool;

    impl crate::tools::types::Tool for SleepTool {
        fn name(&self) -> &str {
            "sleep"
        }
        fn description(&self) -> &str {
            "Sleeps for a given number of milliseconds"
        }
        fn parameters_schema(&self) -> crate::tools::types::ToolSchema {
            crate::tools::types::ToolSchema {
                name: "sleep".to_string(),
                description: String::new(),
                input_type: None,
            }
        }
        fn execute(
            &self,
            params: crate::tools::types::ToolParams,
        ) -> crate::tools::types::ToolResult<crate::tools::types::ToolOutput> {
            let ms = params.get::<u64>("ms").unwrap_or(10);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(crate::tools::types::ToolOutput::success(format!("slept {ms}ms")))
        }
    }

    fn manager_with_sleep_tool() -> ToolManager {
        use crate::tools::registry::{ToolEntry, ToolRegistry};
        use crate::tools::types::TracingToolLogger;
        let logger = Arc::new(TracingToolLogger);
        let registry = ToolRegistry::new(vec![], logger.clone());
        registry
            .register(ToolEntry {
                tool: Arc::new(SleepTool),
                metadata: crate::tools::types::ToolMetadata {
                    name: "sleep".to_string(),
                    version: "0".to_string(),
                    description: "test".to_string(),
                    dependencies: vec![],
                },
                loaded_at: std::time::Instant::now(),
                plugin: None,
            })
            .expect("register sleep tool");
        ToolManager::new(Arc::new(registry))
    }

    #[tokio::test]
    async fn cancelled_token_aborts_slow_tool_quickly() {
        let manager = manager_with_sleep_tool();
        let cancel = CancellationToken::new();
        cancel.cancel(); // pre-cancelled: the cancel branch is ready immediately
        let started = std::time::Instant::now();
        let result = execute_tool_call(
            &manager,
            &cancel,
            "sleep",
            r#"{"ms": 5000}"#,
            &crate::tools::types::ToolProgress::none(),
        )
        .await;
        // The tool itself sleeps 5 s; the select! must have bailed long before.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "cancellation should not wait for the tool to finish"
        );
        assert_eq!(result.as_deref(), Ok("Error: Cancelled (tool 'sleep' aborted)"));
    }

    #[tokio::test]
    async fn bad_args_fail_fast_without_running_tool() {
        let manager = manager_with_sleep_tool();
        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let result = execute_tool_call(
            &manager,
            &cancel,
            "sleep",
            "{not json",
            &crate::tools::types::ToolProgress::none(),
        )
        .await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "parse errors must not wait for a tool run"
        );
        assert!(result.is_err(), "unparseable args are Err, got {result:?}");
    }

    #[tokio::test]
    async fn success_result_passes_through_unmodified() {
        let manager = manager_with_sleep_tool();
        let cancel = CancellationToken::new();
        let result = execute_tool_call(
            &manager,
            &cancel,
            "sleep",
            r#"{"ms": 1}"#,
            &crate::tools::types::ToolProgress::none(),
        )
        .await;
        // Bare string results display verbatim (no JSON quoting), so the
        // model sees the tool's exact output.
        assert_eq!(result.as_deref(), Ok("slept 1ms"));
    }
}
