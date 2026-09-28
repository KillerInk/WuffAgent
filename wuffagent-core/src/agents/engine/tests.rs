//! Unit tests for the `engine` module (see `super`).

/// I4/2a: engine-level — the improvement LLM call happens ONLY on
/// cooldown-boundary task completions (maintenance is disabled so only the
/// improvement gate is exercised; 2a: the cooldown + evidence are now
/// per-AGENT state, so the tagged lesson below is what re-arms "coder").
#[tokio::test]
async fn test_post_task_improvement_llm_called_only_on_boundary() {
    use super::super::LlmClient;
    use super::{AgentEngine, RunParams};
    use crate::agents::config::AgentConfig;
    use crate::agents::RunStats;
    use crate::memory::{MemoryConfig, MemoryEntry, MemoryManager, MemoryType};
    use crate::tools::{ToolManager, ToolRegistry, TracingToolLogger};
    use crate::types::Message;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
    let tool_manager = Arc::new(ToolManager::new(Arc::new(ToolRegistry::new(
        vec![],
        Arc::new(TracingToolLogger),
    ))));
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
    let stats = RunStats {
        tool_calls: 0,
        tool_errors: 0,
        verification_attempts: 0,
    };

    let params = RunParams::default();

    // Task 1: before the boundary -> no LLM call.
    engine
        .post_task_maintenance(&cfg, "task", &Ok("ok".to_string()), stats, &params)
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "task 1 is not a boundary");

    // Task 2: the boundary -> exactly one LLM call (and the check is
    // recorded, baselining the lesson).
    engine
        .post_task_maintenance(&cfg, "task", &Ok("ok".to_string()), stats, &params)
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "task 2 is the boundary");

    // Task 3: after the boundary -> no second call.
    engine
        .post_task_maintenance(&cfg, "task", &Ok("ok".to_string()), stats, &params)
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "task 3 is not a boundary");
}
