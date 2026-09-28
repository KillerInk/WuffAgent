//! Self-improvement tools — the agent-visible side of the improvement loop
//! (the loop itself: `crate::agents::improvement` + the per-task trigger in
//! `AgentEngine::post_task_maintenance`).
//!
//! One file per tool (the builtin convention):
//! - [`status`] — `list_improvement_status`: read-only snapshot of the loop
//!   (last check, evidence gate, settings, lesson count; no LLM call).
//! - [`run`] — `run_self_improvement`: on-demand check reusing the engine's
//!   `suggest_improvements` (bypasses the cooldown/evidence gates; emits
//!   `AppEvent::ImprovementSuggested` for the review panel).
//! - [`metrics`] — `read_metrics`: agent-readable view of the run-metrics
//!   store (windowed aggregates + recent lines; fleet mode when no agent is
//!   named). Read-only, no LLM call.

mod evals;
mod metrics;
mod run;
mod run_eval;
mod status;

pub use evals::{DeleteEvalTool, ListEvalsTool, SaveEvalTool};
pub use metrics::ReadMetricsTool;
pub use run::RunSelfImprovementTool;
pub use run_eval::RunEvalTool;
pub use status::ListImprovementStatusTool;

use std::sync::{Arc, Mutex};

use crate::agents::{AgentManager, LlmClient};
use crate::client::ChatClient;
use crate::memory::MemoryManager;
use crate::tools::registry::ToolRegistry;
use crate::tools::types::{Tool, ToolResult};
use crate::tools::ToolManager;
use crate::types::AppEvent;

/// 2a: register the self-improvement tools. Both are per-profile
/// `allowed_tools`-gated like every builtin; the run tool additionally takes
/// the app's event channel so its suggestions reach the review panel
/// (`session_id` empty = app-level event, routed by the UI to the active
/// session — same convention as the mcp config tools).
pub fn register_improvement_tools(
    registry: &ToolRegistry,
    memory: Arc<MemoryManager>,
    agents: Arc<AgentManager>,
    events: Option<Arc<Mutex<std::sync::mpsc::Sender<AppEvent>>>>,
    llm_client: Arc<dyn LlmClient>,
    session_client: Arc<ChatClient>,
    tool_manager: Arc<Mutex<ToolManager>>,
) -> ToolResult<()> {
    // 2e: read_metrics' default window follows the config knob.
    let window_days = memory.config().improvement_metrics_window_days.max(1) as u64;
    // 2a: one shared default eval store for the eval tools.
    let evals = Arc::new(crate::memory::evals::EvalStore::default());
    for (name, desc, tool) in [
        (
            "list_improvement_status",
            "Show the auto-improvement loop's state (last check, evidence, cooldown)",
            Arc::new(ListImprovementStatusTool::new(memory.clone())) as Arc<dyn Tool>,
        ),
        (
            "read_metrics",
            "Read the run-metrics store: windowed aggregates + recent lines for an agent, or a fleet overview (status=true → fleet loop status)",
            Arc::new(ReadMetricsTool::new()
                .with_memory(memory.clone())
                .with_default_days(window_days)) as Arc<dyn Tool>,
        ),
        (
            "run_self_improvement",
            "Run an on-demand self-improvement check for an agent (bypasses the cooldown)",
            Arc::new(RunSelfImprovementTool::new(
                memory,
                agents.clone(),
                events,
            )) as Arc<dyn Tool>,
        ),
        (
            "save_eval",
            "Save (or upsert by id) a golden/regression eval for an agent (basis for run_eval)",
            Arc::new(SaveEvalTool::new(evals.clone())) as Arc<dyn Tool>,
        ),
        (
            "list_evals",
            "List an agent's saved evals (id, task, verification criteria)",
            Arc::new(ListEvalsTool::new(evals.clone())) as Arc<dyn Tool>,
        ),
        (
            "delete_eval",
            "Delete a saved eval by id for an agent",
            Arc::new(DeleteEvalTool::new(evals.clone())) as Arc<dyn Tool>,
        ),
        (
            "run_eval",
            "Run a profile's saved evals headlessly and report a pass/fail table (recorded as Eval metrics lines)",
            Arc::new(RunEvalTool::new(
                evals,
                agents.clone(),
                llm_client,
                session_client,
                tool_manager,
            )) as Arc<dyn Tool>,
        ),
    ] {
        super::register_tool(registry, name, desc, tool)?;
    }
    Ok(())
}
