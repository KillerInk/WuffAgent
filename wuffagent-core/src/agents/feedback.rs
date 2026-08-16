use super::types::{AgentResult, FeedbackMessage, Task};

/// State of the feedback loop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FeedbackState {
    #[default]
    Running,
    Refining,
    Complete,
    Failed,
}

impl std::fmt::Display for FeedbackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedbackState::Running => write!(f, "running"),
            FeedbackState::Refining => write!(f, "refining"),
            FeedbackState::Complete => write!(f, "complete"),
            FeedbackState::Failed => write!(f, "failed"),
        }
    }
}

/// Feedback loop state machine.
#[derive(Clone, Debug)]
pub struct FeedbackLoop {
    pub state: FeedbackState,
    pub iteration: u32,
    pub max_iterations: u32,
    pub pending_messages: Vec<FeedbackMessage>,
}

impl Default for FeedbackLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackLoop {
    pub fn new() -> Self {
        Self {
            state: FeedbackState::Running,
            iteration: 0,
            max_iterations: 5,
            pending_messages: Vec::new(),
        }
    }

    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Feed a task result into the feedback loop.
    pub fn on_task_result(&mut self, result: AgentResult) {
        self.pending_messages.push(FeedbackMessage::TaskReport { result });
    }

    /// Process pending messages and advance state.
    pub fn tick(&mut self) -> Option<FeedbackMessage> {
        if self.pending_messages.is_empty() {
            return None;
        }

        let message = self.pending_messages.remove(0);
        match &message {
            FeedbackMessage::TaskReport { result } => {
                if result.status == super::types::TaskStatus::Failed && result.needs_refinement {
                    self.iteration += 1;
                    if self.iteration >= self.max_iterations {
                        self.state = FeedbackState::Failed;
                    } else {
                        self.state = FeedbackState::Refining;
                        return Some(FeedbackMessage::RequestRefinement {
                            completed_tasks: vec![],
                            failed_tasks: vec![result.clone()],
                            context: serde_json::Value::Object(serde_json::Map::new()),
                        });
                    }
                }
            }
            FeedbackMessage::RequestRefinement { .. } => {
                self.iteration += 1;
                if self.iteration >= self.max_iterations {
                    self.state = FeedbackState::Failed;
                }
            }
            FeedbackMessage::UpdatedPlan { .. } => {
                self.state = FeedbackState::Running;
            }
            _ => {}
        }
        Some(message)
    }

    /// Check if the loop should stop.
    pub fn is_done(&self) -> bool {
        matches!(self.state, FeedbackState::Complete | FeedbackState::Failed)
    }

    /// Mark the loop as complete.
    pub fn mark_complete(&mut self) {
        self.state = FeedbackState::Complete;
    }

    /// Mark the loop as failed.
    pub fn mark_failed(&mut self) {
        self.state = FeedbackState::Failed;
    }
}

/// Helper: construct a feedback message for task delegation.
pub fn make_delegation(task: Task, context: serde_json::Value) -> FeedbackMessage {
    FeedbackMessage::TaskDelegation { task, context }
}

/// Helper: construct a feedback message for plan update.
pub fn make_plan_update(plan: super::types::ExecutionPlan) -> FeedbackMessage {
    FeedbackMessage::UpdatedPlan { plan }
}

/// Helper: construct a feedback message for objective completion.
pub fn make_objective_complete(output: serde_json::Value) -> FeedbackMessage {
    FeedbackMessage::ObjectiveComplete { final_output: output }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{AgentType, TaskStatus};

    #[test]
    fn test_feedback_loop_new() {
        let loop_ = FeedbackLoop::new();
        assert_eq!(loop_.state, FeedbackState::Running);
        assert_eq!(loop_.iteration, 0);
        assert!(loop_.pending_messages.is_empty());
        assert!(!loop_.is_done());
    }

    #[test]
    fn test_feedback_loop_tick_with_no_messages() {
        let mut loop_ = FeedbackLoop::new();
        assert!(loop_.tick().is_none());
    }

    #[test]
    fn test_feedback_loop_mark_complete() {
        let mut loop_ = FeedbackLoop::new();
        loop_.mark_complete();
        assert!(loop_.is_done());
    }

    #[test]
    fn test_feedback_loop_max_iterations() {
        let mut loop_ = FeedbackLoop::new();
        loop_.max_iterations = 2;

        // Simulate two refinement requests
        loop_.pending_messages.push(FeedbackMessage::RequestRefinement {
            completed_tasks: vec![],
            failed_tasks: vec![],
            context: serde_json::Value::Object(serde_json::Map::new()),
        });
        loop_.tick();
        assert_eq!(loop_.iteration, 1);
        assert!(!loop_.is_done());

        loop_.pending_messages.push(FeedbackMessage::RequestRefinement {
            completed_tasks: vec![],
            failed_tasks: vec![],
            context: serde_json::Value::Object(serde_json::Map::new()),
        });
        loop_.tick();
        assert_eq!(loop_.iteration, 2);
        assert!(loop_.is_done());
    }
}
