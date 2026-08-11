use std::sync::Arc;
use std::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio::sync::Mutex;
use tracing;

use super::planner::PlannerAgent;
use super::supervisor::SupervisorAgent;
use super::traits::{AgentError, ChatClientLike, PlannerAgent as PlannerAgentTrait, SupervisorAgent as SupervisorAgentTrait, SupervisorDecision};
use super::types::{AgentResult, Task, TaskStatus};
use super::types::build_context;
use crate::types::AppEvent;

/// The top-level orchestrator that runs the full multi-agent pipeline.
pub struct AgentPipeline<C: ChatClientLike> {
    planner: Arc<Mutex<PlannerAgent<C>>>,
    supervisor: Arc<SupervisorAgent>,
    max_iterations: u32,
    cancel_token: Arc<CancellationToken>,
    event_tx: Option<Arc<std::sync::Mutex<mpsc::Sender<AppEvent>>>>,
}

impl<C: ChatClientLike + 'static> AgentPipeline<C> {
    pub fn new(
        planner: PlannerAgent<C>,
        supervisor: Arc<SupervisorAgent>,
        event_tx: Option<Arc<std::sync::Mutex<mpsc::Sender<AppEvent>>>>,
    ) -> Self {
        Self {
            planner: Arc::new(Mutex::new(planner)),
            supervisor,
            max_iterations: 5,
            cancel_token: Arc::new(CancellationToken::new()),
            event_tx,
        }
    }

    /// Execute the full pipeline for a user request.
    pub async fn execute(
        &self,
        user_request: &str,
    ) -> Result<Vec<AgentResult>, AgentError> {
        tracing::info!("Pipeline starting for request: {}", user_request);

        let mut plan = {
            let planner = self.planner.lock().await;
            PlannerAgentTrait::generate_plan(&*planner, user_request, None).await?
        };

        let mut all_results = Vec::new();
        let mut iteration = 0u32;
        // Track task IDs that have failed at least once to avoid infinite retry loops
        let mut failed_task_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            if self.cancel_token.is_cancelled() {
                tracing::info!("Pipeline cancelled");
                return Err(AgentError::Cancelled);
            }

            if iteration >= self.max_iterations {
                tracing::warn!("Max iterations ({}) exceeded", self.max_iterations);
                return Err(AgentError::PlanError(
                    "Maximum refinement iterations exceeded".to_string(),
                ));
            }

            tracing::info!("Pipeline iteration {}: executing plan '{}'", iteration, plan.plan_id);

            let (decision, new_results) = SupervisorAgentTrait::execute_plan(&*self.supervisor, &plan).await?;

            match decision {
                SupervisorDecision::Complete { final_output } => {
                    tracing::info!("Pipeline complete. Output: {}", final_output);
                    all_results.extend(new_results);
                    let output_str = match final_output.as_str() {
                        Some(s) if !s.is_empty() => s.to_string(),
                        _ => format!("{}", final_output),
                    };
                    self.send_event(AppEvent::AgentPipelineComplete {
                        result_count: all_results.len(),
                        final_output: output_str,
                    });
                    return Ok(all_results);
                }
                SupervisorDecision::Refine {
                    completed,
                    failed,
                    context,
                } => {
                    iteration += 1;
                    tracing::info!(
                        "Refining plan: {} completed, {} failed, iteration {}/{}",
                        completed.len(),
                        failed.len(),
                        iteration,
                        self.max_iterations
                    );
                    self.send_event(AppEvent::AgentFeedbackLoop {
                        iteration,
                        action: format!("refining: {} completed, {} failed", completed.len(), failed.len()),
                    });

                    // Track failed task IDs to avoid infinite retry loops
                    for result in &failed {
                        failed_task_ids.insert(result.task_id.clone());
                    }
                    let refined_plan = {
                        let planner = self.planner.lock().await;
                        PlannerAgentTrait::refine_plan(&*planner, &plan, &completed, &failed, &context).await?
                    };
                    plan = refined_plan;
                    all_results.extend(completed);
                }
                SupervisorDecision::Retry { tasks } => {
                    // Filter out tasks that have already failed once — don't retry forever
                    let retryable: Vec<Task> = tasks
                        .iter()
                        .filter(|t| !failed_task_ids.contains(&t.id))
                        .cloned()
                        .collect();
                    if retryable.is_empty() {
                        tracing::warn!("All retryable tasks have already failed, giving up");
                        // Mark remaining as failed and complete
                        for task in &tasks {
                            failed_task_ids.insert(task.id.clone());
                        }
                        // Fall through to complete with what we have
                        let final_output = build_context(&all_results);
                        self.send_event(AppEvent::AgentPipelineComplete {
                            result_count: all_results.len(),
                            final_output: format!("{}", final_output),
                        });
                        return Ok(all_results);
                    }
                    tracing::info!("Retrying {} failed tasks ({} already failed before)", retryable.len(), tasks.len() - retryable.len());
                    let retry_results = self.supervisor.retry_tasks(&retryable, &all_results).await?;
                    // Track which tasks failed this round
                    for result in &retry_results {
                        if result.status == TaskStatus::Failed {
                            failed_task_ids.insert(result.task_id.clone());
                        }
                    }
                    all_results.extend(retry_results);
                }
                SupervisorDecision::Continue { new_tasks } => {
                    tracing::info!("Adding {} suggested follow-up tasks", new_tasks.len());
                    plan.tasks.extend(new_tasks);
                }
            }
        }
    }

    /// Cancel the running pipeline.
    pub fn cancel(&self) {
        tracing::info!("Pipeline cancel requested");
        self.cancel_token.cancel();
    }

    fn send_event(&self, event: AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }
}
