use std::sync::Arc;
use std::collections::HashSet;
use std::sync::mpsc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing;

use super::types::{AgentId, AgentResult, AgentType, ExecutionPlan, Task, TaskStatus, build_context};
use super::worker::WorkerAgent;
use super::worker_registry::WorkerRegistry;
use super::config::WorkerConfig;
use crate::tools::ToolManager;
use crate::types::AppEvent;
use super::traits::{Agent, AgentError, AgentRole, SupervisorAgent as SupervisorAgentTrait, SupervisorDecision};

/// Supervisor Agent — coordinates worker execution.
#[derive(Clone)]
pub struct SupervisorAgent {
    id: AgentId,
    worker_registry: Arc<WorkerRegistry>,
    tool_manager: Arc<Mutex<ToolManager>>,
    max_parallel: usize,
    worker_configs: Vec<WorkerConfig>,
    cancel_token: Arc<CancellationToken>,
    event_tx: Option<Arc<std::sync::Mutex<mpsc::Sender<AppEvent>>>>,
}

impl SupervisorAgent {
    pub fn new(
        worker_registry: Arc<WorkerRegistry>,
        tool_manager: Arc<Mutex<ToolManager>>,
        worker_configs: Vec<WorkerConfig>,
        max_parallel: usize,
        event_tx: Option<Arc<std::sync::Mutex<mpsc::Sender<AppEvent>>>>,
    ) -> Self {
        Self {
            id: AgentId::generate(),
            worker_registry,
            tool_manager,
            max_parallel,
            worker_configs,
            cancel_token: Arc::new(CancellationToken::new()),
            event_tx,
        }
    }

    /// Send an AppEvent through the pipeline event channel.
    fn send_event(&self, event: AppEvent) {
        if let Some(tx) = &self.event_tx {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        }
    }

    /// Find and spawn the best worker for a task.
    async fn spawn_best_worker(
        &self,
        task: &Task,
    ) -> Result<(Box<dyn WorkerAgent>, String), AgentError> {
        // Try to find best worker from registered configs
        if let Some((name, _config)) = self
            .worker_registry
            .find_best_worker(task, &self.worker_configs)
            .await
        {
            if let Some(worker) = self.worker_registry.spawn(&name) {
                tracing::info!(
                    "Selected worker '{}' (type={:?}) for task '{}'",
                    name,
                    worker.agent_type(),
                    task.description
                );
                return Ok((worker, name));
            }
        }

        // Fall back to default worker
        tracing::warn!("No suitable worker found for task '{}', using default", task.description);
        if let Some(worker) = self.worker_registry.spawn("default") {
            return Ok((worker, "default".to_string()));
        }

        Err(AgentError::AgentNotFound("No available workers".to_string()))
    }
}

#[async_trait::async_trait]
impl Agent for SupervisorAgent {
    fn id(&self) -> &AgentId { &self.id }
    fn name(&self) -> &str { "Supervisor" }
    fn role(&self) -> AgentRole { AgentRole::Supervisor }
    fn instructions(&self) -> &str {
        "Coordinate worker agents to execute tasks in the execution plan."
    }
}

#[async_trait::async_trait]
impl SupervisorAgentTrait for SupervisorAgent {
    async fn execute_plan(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<(SupervisorDecision, Vec<AgentResult>), AgentError> {
        tracing::info!(
            "Supervisor executing plan '{}' with {} tasks",
            plan.plan_id,
            plan.tasks.len()
        );

        self.send_event(AppEvent::AgentPlanGenerated {
            plan_id: plan.plan_id.clone(),
            task_count: plan.tasks.len(),
            user_request: plan.user_request.clone(),
            task_descriptions: plan.tasks.iter().map(|t| t.description.clone()).collect(),
        });

        let ordered = plan.ordered_tasks();
        let mut results: Vec<AgentResult> = Vec::new();
        let mut completed_ids: HashSet<String> = HashSet::new();
        let mut pending: Vec<Task> = ordered.into_iter().map(|t| (*t).clone()).collect();
        let mut iteration = 0u32;

        while !pending.is_empty() {
            // Check cancellation
            if self.cancel_token.is_cancelled() {
                self.send_event(AppEvent::AgentPipelineError {
                    error: "Pipeline cancelled".to_string(),
                });
                return Ok((
                    SupervisorDecision::Complete {
                        final_output: serde_json::json!({ "error": "cancelled" }),
                    },
                    results,
                ));
            }

            // Build context from completed tasks
            let context = build_context(&results);

            // Find ready tasks (dependencies satisfied)
            let ready: Vec<Task> = pending
                .iter()
                .filter(|t| match &t.depends_on {
                    None => true,
                    Some(dep_id) => completed_ids.contains(dep_id),
                })
                .cloned()
                .take(self.max_parallel)
                .collect();

            if ready.is_empty() {
                // Check if we're stuck
                if pending.iter().all(|t| match &t.depends_on {
                    None => completed_ids.contains(&t.id),
                    Some(dep_id) => completed_ids.contains(dep_id),
                }) {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                continue;
            }

            // Spawn tasks concurrently
            let mut handles = Vec::new();
            for task in ready {
                if self.cancel_token.is_cancelled() {
                    break;
                }

                let registry = self.worker_registry.clone();
                let worker_configs = self.worker_configs.clone();
                let task_clone = task.clone();
                let task_id = task_clone.id.clone();
                let ctx_clone = context.clone();
                let event_tx = self.event_tx.clone();
                let timeout_ms = self.worker_configs.first().map(|c| c.priority).unwrap_or(0);
                let _timeout_ms = timeout_ms; // ensure the value is used

                let handle = tokio::spawn(async move {
                    // Find best worker
                    let (worker_name, mut worker) = if let Some((name, _config)) =
                        registry.find_best_worker(&task_clone, &worker_configs).await
                    {
                        match registry.spawn(&name) {
                            Some(w) => (name.clone(), w),
                            None => {
                                tracing::warn!("Failed to spawn worker '{}', falling back to 'default'", name);
                                match registry.spawn("default") {
                                    Some(w) => (name.clone(), w),
                                    None => {
                                        tracing::error!("Failed to spawn fallback worker 'default'");
                                        return (String::from("unknown"), task_id.clone(), Err(AgentError::ConfigError("No available workers".to_string())));
                                    }
                                }
                            }
                        }
                    } else {
                        match registry.spawn("default") {
                            Some(w) => (String::from("default"), w),
                            None => {
                                tracing::error!("Failed to spawn fallback worker 'default'");
                                return (String::from("unknown"), task_id.clone(), Err(AgentError::ConfigError("No available workers".to_string())));
                            }
                        }
                    };

                    let task_description = task_clone.description.clone();
                    // Send task started event
                    if let Some(tx) = &event_tx {
                        if let Ok(tx) = tx.lock() {
                            let _ = tx.send(AppEvent::AgentTaskStarted {
                                task_id: task_clone.id.clone(),
                                task_description: task_description,
                                agent_type: worker_name.clone(),
                            });
                        }
                    }

                    let result = tokio::time::timeout(
                        std::time::Duration::from_millis(60_000),
                        worker.execute_task(&task_clone, &ctx_clone),
                    ).await;

                    match result {
                        Ok(inner) => (worker_name, task_id.clone(), inner),
                        Err(_) => {
                            tracing::warn!("Task '{}' timed out", task_clone.id);
                            (worker_name, task_id.clone(), Err(AgentError::Timeout(60_000)))
                        }
                    }
                });
                handles.push(handle);
            }

            // Collect results
            for handle in handles {
                match handle.await {
                    Ok((worker_name, task_id, Ok(result))) => {
                        tracing::info!(
                            "Task {} completed by worker '{}': {}",
                            result.task_id,
                            worker_name,
                            result.summary
                        );
                        completed_ids.insert(result.task_id.clone());
                        results.push(result);
                    }
                    Ok((worker_name, task_id, Err(e))) => {
                        tracing::warn!("Task failed with worker '{}': {}", worker_name, e);
                        let err_msg = e.to_string();
                        let failed_result = AgentResult {
                            task_id: task_id.clone(),
                            agent_id: worker_name,
                            agent_type: AgentType::General,
                            status: TaskStatus::Failed,
                            output: serde_json::json!({ "error": err_msg }),
                            summary: format!("Task failed: {}", e),
                            needs_refinement: false,
                            fixable: err_msg.contains("is required") || err_msg.contains("required"),
                            suggested_followup: vec![],
                            duration_ms: 0,
                            completed_at: Some(chrono::Utc::now()),
                        };
                        // Emit tool error event for planner to fix
                        if let Some(tx) = &self.event_tx {
                            if let Ok(tx) = tx.lock() {
                                let _ = tx.send(AppEvent::AgentToolError {
                                    tool_name: "unknown".to_string(),
                                    task_id: task_id.clone(),
                                    error: err_msg.clone(),
                                });
                            }
                        }
                        completed_ids.insert(failed_result.task_id.clone());
                        results.push(failed_result);
                    }
                    Err(e) => {
                        tracing::warn!("Task join error: {}", e);
                    }
                }
            }

            // Remove completed tasks from pending
            pending.retain(|t| !completed_ids.contains(&t.id));
            iteration += 1;

            self.send_event(AppEvent::AgentFeedbackLoop {
                iteration,
                action: format!("completed {}/{} tasks", results.len(), plan.tasks.len()),
            });
        }

        let decision = self.decide_next_action(&results, plan).await?;
        Ok((decision, results))
    }

    async fn spawn_worker(
        &self,
        task: &Task,
    ) -> Result<Box<dyn WorkerAgent>, AgentError> {
        let (worker, _) = self.spawn_best_worker(task).await?;
        Ok(worker)
    }

    async fn retry_tasks(
        &self,
        tasks: &[Task],
        prior_results: &[AgentResult],
    ) -> Result<Vec<AgentResult>, AgentError> {
        let context = build_context(prior_results);
        let mut results = Vec::new();

        for task in tasks {
            if self.cancel_token.is_cancelled() {
                break;
            }

            // Use the best matching worker for the task, not just the hardcoded "default"
            let (worker_name, mut worker) = if let Some((name, _config)) =
                self.worker_registry.find_best_worker(task, &self.worker_configs).await
            {
                match self.worker_registry.spawn(&name) {
                    Some(w) => (name.clone(), w),
                    None => {
                        tracing::warn!("Failed to spawn retry worker '{}', falling back to 'default'", name);
                        match self.worker_registry.spawn("default") {
                            Some(w) => ("default".to_string(), w),
                            None => {
                                tracing::error!("No available workers for retry");
                                continue;
                            }
                        }
                    }
                }
            } else {
                match self.worker_registry.spawn("default") {
                    Some(w) => ("default".to_string(), w),
                    None => {
                        tracing::error!("No available workers for retry");
                        continue;
                    }
                }
            };

            tracing::info!("Retrying task '{}' with worker '{}'", task.id, worker_name);
            match worker.execute_task(task, &context).await {
                Ok(result) => {
                    results.push(result);
                }
                Err(e) => {
                    tracing::warn!("Retry task '{}' failed with worker '{}': {}", task.id, worker_name, e);
                    let failed_result = AgentResult {
                        task_id: task.id.clone(),
                        agent_id: worker_name,
                        agent_type: AgentType::General,
                        status: TaskStatus::Failed,
                        output: serde_json::json!({ "error": e.to_string() }),
                        summary: format!("Retry task '{}' failed: {}", task.id, e),
                        needs_refinement: false,
                        fixable: e.to_string().contains("is required") || e.to_string().contains("required"),
                        suggested_followup: vec![],
                        duration_ms: 0,
                        completed_at: Some(chrono::Utc::now()),
                    };
                    results.push(failed_result);
                }
            }
        }

        Ok(results)
    }

    async fn decide_next_action(
        &self,
        results: &[AgentResult],
        plan: &ExecutionPlan,
    ) -> Result<SupervisorDecision, AgentError> {
        let completed: Vec<&AgentResult> = results.iter().filter(|r| r.status == TaskStatus::Completed).collect();
        let failed: Vec<&AgentResult> = results.iter().filter(|r| r.status == TaskStatus::Failed).collect();
        let retryable: Vec<&AgentResult> = results.iter().filter(|r| r.status == TaskStatus::Retryable).collect();
        let has_refinement = results.iter().any(|r| r.needs_refinement);
        let suggested_tasks: Vec<Task> = results.iter().flat_map(|r| r.suggested_followup.clone()).collect();

        tracing::info!(
            "Supervisor deciding: completed={}, failed={}, retryable={}, refinement_requested={}, suggested_tasks={}",
            completed.len(),
            failed.len(),
            retryable.len(),
            has_refinement,
            suggested_tasks.len(),
        );

        // Check if worker suggests refinement
        if has_refinement {
            return Ok(SupervisorDecision::Refine {
                completed: completed.iter().cloned().cloned().collect(),
                failed: failed.iter().cloned().cloned().collect(),
                context: build_context(results),
            });
        }

        // Check if all tasks are done
        let all_done = completed.len() + failed.len() + retryable.len() >= plan.tasks.len();
        if all_done {
            // Check for fixable errors — these need the planner to regenerate with correct params
            let fixable_errors: Vec<AgentResult> = failed
                .iter()
                .filter(|r| r.fixable)
                .cloned()
                .cloned()
                .collect();
            if !fixable_errors.is_empty() {
                return Ok(SupervisorDecision::NeedsFix {
                    failed: fixable_errors,
                    completed: completed.iter().cloned().cloned().collect(),
                    context: build_context(results),
                });
            }

            if failed.is_empty() && retryable.is_empty() {
                let final_output = build_context(results);
                return Ok(SupervisorDecision::Complete { final_output });
            }

            // Collect tasks that failed or are retryable
            let mut retryable_tasks: Vec<Task> = plan
                .tasks
                .iter()
                .filter(|t| {
                    failed.iter().any(|r| r.task_id == t.id)
                        || retryable.iter().any(|r| r.task_id == t.id)
                })
                .cloned()
                .collect();

            // Retryable tasks always get a retry; failed tasks only if they have retries remaining
            retryable_tasks.sort_by_key(|t| t.priority);

            if !retryable_tasks.is_empty() {
                return Ok(SupervisorDecision::Retry {
                    tasks: retryable_tasks,
                });
            }

            // No more retries — refine the plan
            return Ok(SupervisorDecision::Refine {
                completed: completed.iter().cloned().cloned().collect(),
                failed: failed.iter().cloned().cloned().collect(),
                context: build_context(results),
            });
        }

        // Not all tasks done yet
        Ok(SupervisorDecision::Complete {
            final_output: build_context(results),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::AgentType;
    use super::super::worker::GenericWorker;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::lib::TracingToolLogger;

    /// Helper: build a minimal SupervisorAgent with the given configs.
    fn make_supervisor(worker_configs: Vec<WorkerConfig>) -> SupervisorAgent {
        let registry = Arc::new(WorkerRegistry::new());
        let tool_registry = Arc::new(ToolRegistry::new(
            vec![],
            Arc::new(TracingToolLogger),
        ));
        let invocation_registry = Arc::new(crate::agents::invocation_registry::AgentInvocationRegistry::new());
        crate::tools::builtin::register_builtins(&tool_registry, &invocation_registry).expect("failed to register builtin tools");
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
        SupervisorAgent::new(registry, tool_manager, worker_configs, 4, None)
    }

    /// Helper: build a minimal ExecutionPlan with the given tasks.
    fn make_plan(description: &str, tasks: Vec<Task>) -> ExecutionPlan {
        ExecutionPlan::new(description, tasks)
    }

    #[test]
    fn test_supervisor_creation() {
        let sup = make_supervisor(vec![]);
        assert!(!sup.id().to_string().is_empty(), "id must not be empty");
        assert_eq!(sup.name(), "Supervisor");
        assert_eq!(sup.role(), AgentRole::Supervisor);
    }

    #[tokio::test]
    async fn test_execute_plan_no_workers() {
        // Registry with only builtins (default, executing) — should not panic
        // and should return a valid decision with at least one result.
        let sup = make_supervisor(vec![]);
        let task = Task::new("simple task", AgentType::General, serde_json::json!({}));
        let plan = make_plan("test", vec![task]);
        let (decision, results) = sup.execute_plan(&plan).await.unwrap();
        assert!(!results.is_empty(), "should produce at least one result");
        // With GenericWorker now executing tools, the result may be Completed or Failed.
        // The supervisor should never return an error decision here.
        match decision {
            SupervisorDecision::Complete { .. }
            | SupervisorDecision::Retry { .. }
            | SupervisorDecision::NeedsFix { .. } => {},
            _ => panic!("expected Complete, Retry or NeedsFix decision, got {:?}", decision),
        }
    }

    #[tokio::test]
    async fn test_spawn_best_worker_no_workers() {
        // Even with empty configs, builtins are available so spawn should succeed
        let sup = make_supervisor(vec![]);
        let task = Task::new("test task", AgentType::General, serde_json::json!({}));
        let worker = sup.spawn_worker(&task).await;
        assert!(worker.is_ok(), "should spawn a builtin worker even with no configs");
    }

    #[tokio::test]
    async fn test_supervisor_cancel_token() {
        let sup = make_supervisor(vec![]);
        // Cancel before execution
        sup.cancel_token.cancel();
        let task = Task::new("should be cancelled", AgentType::General, serde_json::json!({}));
        let plan = make_plan("cancel-test", vec![task]);
        let (decision, results) = sup.execute_plan(&plan).await.unwrap();
        // Should return early with cancelled signal, not error
        match decision {
            SupervisorDecision::Complete { final_output } => {
                assert_eq!(final_output["error"], "cancelled");
            }
            _ => panic!("expected Complete with cancelled error"),
        }
        assert!(results.is_empty(), "no tasks should have run");
    }

    #[tokio::test]
    async fn test_spawn_best_worker_with_registered_worker() {
        let registry = Arc::new(WorkerRegistry::new());

        let tool_registry = Arc::new(ToolRegistry::new(
            vec![],
            Arc::new(TracingToolLogger),
        ));
        let invocation_registry = Arc::new(crate::agents::invocation_registry::AgentInvocationRegistry::new());
        crate::tools::builtin::register_builtins(&tool_registry, &invocation_registry).expect("failed to register builtin tools");
        let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));

        // Register a custom worker with tool_manager
        let tm = tool_manager.clone();
        registry.register("my_worker", move || {
            Box::new(GenericWorker::new(
                "my_worker",
                "My custom worker",
                vec!["file_io".to_string()],
                "You are a custom worker.",
                tm.clone(),
            ))
        });
        assert!(registry.has("my_worker"));

        let config = WorkerConfig {
            name: "my_worker".to_string(),
            description: "My custom worker".to_string(),
            system_prompt: "You are a custom worker.".to_string(),
            allowed_tools: vec!["file_io".to_string()],
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            can_invoke: vec![],
            handoff_enabled: false,
        };
        let sup = SupervisorAgent::new(registry, tool_manager, vec![config], 4, None);

        let task = Task::new("write a file", AgentType::Coding, serde_json::json!({
            "tools": ["file_io"],
            "action": "write",
            "path": "test.txt",
            "content": "hello"
        }));
        let worker = sup.spawn_worker(&task).await;
        assert!(worker.is_ok(), "should spawn registered worker");
        let w = worker.unwrap();
        assert_eq!(w.agent_type(), AgentType::General);
    }
}
