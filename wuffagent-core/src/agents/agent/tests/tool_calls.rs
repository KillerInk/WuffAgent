//! M1: per-run tool counters (regression).
//!
//! The counters are incremented as the calls execute inside
//! `run_native_tool_calls` / `run_text_embedded_calls`. This replaces the
//! old `run_stats_since` re-scan of the request list, which came out
//! all-zero once mid-run trimming invalidated its start-of-run offset on
//! long turns (the observed all-zero metrics line).

use super::*;
use crate::agents::types::RunStats;
use std::collections::HashSet;

/// An agent whose shared registry holds the `calculation` tool: `2 + 3`
/// succeeds, `sqrt(-1)` fails with a tool error.
fn make_agent_with_calc() -> Agent {
    let config = AgentConfig {
        name: "test".to_string(),
        ..Default::default()
    };
    let registry = ToolRegistry::new(vec![], Arc::new(TracingToolLogger));
    registry
        .register(crate::tools::registry::ToolEntry {
            tool: Arc::new(crate::tools::builtin::calculation::CalculationTool::new()),
            metadata: crate::tools::types::ToolMetadata {
                name: "calculation".to_string(),
                version: "1.0.0".to_string(),
                description: "Evaluate mathematical expressions".to_string(),
                dependencies: vec![],
            },
            loaded_at: std::time::Instant::now(),
            plugin: None,
        })
        .unwrap();
    let tool_manager = Arc::new(ToolManager::new(Arc::new(registry)));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    Agent::builder(config, Arc::new(NoopLlm), client)
        .tool_manager(tool_manager)
        .build()
}

fn calc_call(id: &str, expression: &str) -> crate::types::ToolCall {
    crate::types::ToolCall {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: crate::types::ToolFunction {
            name: "calculation".to_string(),
            arguments: format!(r#"{{"expression": "{expression}"}}"#),
        },
    }
}

#[tokio::test]
async fn test_native_tool_calls_accumulate_run_counters() {
    let agent = make_agent_with_calc();
    let tm = agent.tool_manager.as_ref().clone();
    let pending = super::tool_exec::PendingToolRuns::new(
        super::tool_exec::EventSink::new(None, "test".to_string()),
        tm.clone(),
        CancellationToken::new(),
    );
    let mut messages = vec![test_msg("user", "do the math")];
    let mut counters = RunStats::default();
    let calls = vec![calc_call("c1", "2 + 3"), calc_call("c2", "sqrt(-1)")];

    super::super::tool_calls::run_native_tool_calls(
        &agent,
        &calls,
        &pending,
        &CancellationToken::new(),
        &HashSet::new(),
        &mut messages,
        &tm,
        &mut counters,
    )
    .await
    .expect("run completes");

    assert_eq!(counters.tool_calls, 2, "both calls were executed");
    assert_eq!(counters.tool_errors, 1, "sqrt(-1) is the only error");
    // Results recorded in call order (assistant + tool message per call).
    assert_eq!(messages.len(), 3, "assistant + tool results recorded");
    let err_result = messages
        .iter()
        .find(|m| m.role == "tool" && m.content.starts_with("Error: "))
        .expect("the failing call's tool result is recorded");
    assert_eq!(err_result.tool_call_id.as_deref(), Some("c2"));
}

#[tokio::test]
async fn test_text_embedded_calls_accumulate_run_counters() {
    let agent = make_agent_with_calc();
    let tm = agent.tool_manager.as_ref().clone();
    let pending = super::tool_exec::PendingToolRuns::new(
        super::tool_exec::EventSink::new(None, "test".to_string()),
        tm.clone(),
        CancellationToken::new(),
    );
    let tool_defs = Some(tm.get_tool_definitions());
    let mut messages = vec![test_msg("user", "do the math")];
    let mut counters = RunStats::default();

    // A JSON array embedded in prose — the bracket-region extraction path.
    let display = "I'll check both.\n[\
        {\"id\": \"e1\", \"type\": \"function\", \"function\": {\"name\": \"calculation\", \"arguments\": \"{\\\"expression\\\": \\\"2 + 3\\\"}\"}},\
        {\"id\": \"e2\", \"type\": \"function\", \"function\": {\"name\": \"calculation\", \"arguments\": \"{\\\"expression\\\": \\\"sqrt(-1)\\\"}\"}}\
        ]\ndone.";
    let ran = super::super::tool_calls::run_text_embedded_calls(
        &agent,
        &tool_defs,
        display,
        &CancellationToken::new(),
        &pending,
        &mut messages,
        &tm,
        &mut counters,
    )
    .await
    .expect("run completes");

    assert!(ran, "embedded calls were executed");
    assert_eq!(counters.tool_calls, 2);
    assert_eq!(counters.tool_errors, 1, "sqrt(-1) is the only error");
    // This path records an assistant message per call (unlike the native
    // path, whose assistant message is pushed by the stream loop):
    // user + 2×(assistant + tool) = 5.
    assert_eq!(messages.len(), 5, "assistant + tool results recorded");
}
