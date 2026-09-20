//! Unit tests for the `engine` module (see `super`).

use super::improvement_due;

/// I4: the improvement check fires only on cooldown boundaries, and a
/// misconfigured cooldown of 0 degrades to "every task" instead of a
/// division-by-zero.
#[test]
fn test_improvement_due_respects_cooldown() {
    let cooldown = 5;
    assert!(improvement_due(5, cooldown), "boundary task 5");
    assert!(improvement_due(10, cooldown), "boundary task 10");
    assert!(!improvement_due(4, cooldown), "task 4 before boundary");
    assert!(!improvement_due(6, cooldown), "task 6 after boundary");
    assert!(!improvement_due(1, cooldown), "task 1");

    // Cooldown 0 is treated as 1 (every task) via `.max(1)`.
    assert!(improvement_due(1, 0));
    assert!(improvement_due(42, 0));
}

/// I4: engine-level — the improvement LLM call happens ONLY on
/// cooldown-boundary task completions (maintenance is disabled so only the
/// improvement gate is exercised).
#[tokio::test]
async fn test_post_task_improvement_llm_called_only_on_boundary() {
    use super::super::LlmClient;
    use super::AgentEngine;
    use crate::agents::config::AgentConfig;
    use crate::agents::RunStats;
    use crate::memory::{MemoryConfig, MemoryEntry, MemoryManager, MemoryType};
    use crate::tools::{ToolManager, ToolRegistry, TracingToolLogger};
    use crate::types::Message;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// LLM that counts how often it is asked for anything.
    struct CountingLlm(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl LlmClient for CountingLlm {
        async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("[]".to_string())
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _chunk: Box<dyn FnMut(String) + Send + Sync + 'static>,
        ) -> Result<String, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("[]".to_string())
        }
    }

    // Memory: auto_improve ON (the I4 default), cooldown of 2, maintenance
    // off.
    let dir = tempfile::tempdir().unwrap();
    let config = MemoryConfig {
        auto_improve: true,
        improvement_cooldown_tasks: 2,
        memory_maintenance: false,
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let memory = Arc::new(MemoryManager::new(config).unwrap());
    // One lesson so the evidence gate passes whenever the cooldown allows.
    memory
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson that counts as evidence",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(Arc::new(ToolRegistry::new(
        vec![],
        Arc::new(TracingToolLogger),
    )))));
    let engine = AgentEngine::new(
        Arc::new(CountingLlm(calls.clone())),
        tool_manager,
        Arc::new(crate::client::ChatClient::new("http://localhost:1")),
    )
    .with_memory(memory);

    let cfg = AgentConfig {
        name: "coder".to_string(),
        system_prompt: "You are a coding agent.".to_string(),
        ..Default::default()
    };
    let stats = RunStats { tool_calls: 0, tool_errors: 0, verification_attempts: 0 };

    // Task 1: before the boundary -> no LLM call.
    engine
        .post_task_maintenance(&cfg, "task", &Ok("ok".to_string()), stats)
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "task 1 is not a boundary");

    // Task 2: the boundary -> exactly one LLM call (and the check is
    // recorded, baselining the lesson).
    engine
        .post_task_maintenance(&cfg, "task", &Ok("ok".to_string()), stats)
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "task 2 is the boundary");

    // Task 3: after the boundary -> no second call.
    engine
        .post_task_maintenance(&cfg, "task", &Ok("ok".to_string()), stats)
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "task 3 is not a boundary");
}
