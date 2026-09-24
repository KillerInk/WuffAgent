//! Unit tests for the agent module (see super), split by area:
//! - schema.rs: agent creation, shell schema, per-agent reasoning effort
//! - handoff.rs: handoff tool injection + take_pending_handoff
//! - verify.rs: verify_or_complete (judge) + trim reconciliation + nudge capture
//! - outcomes.rs: record_verification_outcome storage/dedup
//! - context.rs: storable-nudge, request overhead, truncated tool-call repair

use super::*;
use super::verify::record_verification_outcome;
use tokio_util::sync::CancellationToken;
use crate::memory::MemoryType;
use crate::tools::registry::ToolRegistry;
use crate::tools::types::TracingToolLogger;

mod context;
mod handoff;
mod outcomes;
mod schema;
mod verify;

fn make_agent(name: &str) -> Agent {
    let config = AgentConfig {
        name: name.to_string(),
        ..Default::default()
    };
    let llm_client = Arc::new(NoopLlm);
    let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    Agent::new(config, llm_client, tool_manager, None, client, None, None)
}

/// Build an agent whose shared registry contains a `shell` tool, like the
/// global `register_builtins` registration, with `shell_enabled` set.
fn make_agent_with_shell(shell_enabled: bool) -> Agent {
    let mut config = AgentConfig {
        name: "test".to_string(),
        ..Default::default()
    };
    config.shell_config.shell_enabled = shell_enabled;
    let llm_client = Arc::new(NoopLlm);
    let registry = ToolRegistry::new(vec![], Arc::new(TracingToolLogger));
    registry
        .register(crate::tools::registry::ToolEntry {
            tool: Arc::new(crate::tools::builtin::shell::ShellTool::new(
                crate::tools::builtin::shell::ShellConfig {
                    enabled: true,
                    ..Default::default()
                },
            )),
            metadata: crate::tools::types::ToolMetadata {
                name: "shell".to_string(),
                version: "1.0.0".to_string(),
                description: "Execute shell commands on the local system".to_string(),
                dependencies: vec![],
            },
            loaded_at: std::time::Instant::now(),
            plugin: None,
        })
        .unwrap();
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(Arc::new(registry))));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    Agent::new(config, llm_client, tool_manager, None, client, None, None)
}

fn test_msg(role: &str, content: &str) -> Message {
    Message {
        role: role.to_string(),
        content: content.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }
}

/// An LLM that fails every call — proves verification short-circuits
/// without any LLM round-trip when no tool outputs exist.
struct RefuseLlm;
#[async_trait::async_trait]
impl LlmClient for RefuseLlm {
    async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
        Err("LLM must not be called".to_string())
    }
    async fn stream(
        &self,
        _messages: &[Message],
        _chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        Err("LLM must not be called".to_string())
    }
}

/// An LLM that returns a fixed verdict and records the prompt it was
/// given, so tests can assert what the judge actually sees.
struct JudgeLlm {
    verdict: &'static str,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
}
#[async_trait::async_trait]
impl LlmClient for JudgeLlm {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.seen.lock().unwrap().extend_from_slice(messages);
        Ok(self.verdict.to_string())
    }
    async fn stream(
        &self,
        messages: &[Message],
        _chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        // Mirror `complete`: verification goes through the streaming path
        // (the judge call uses no total HTTP timeout), so the mock must
        // record the prompt and return the verdict here too.
        self.seen.lock().unwrap().extend_from_slice(messages);
        Ok(self.verdict.to_string())
    }
}

fn agent_with_llm(llm: std::sync::Arc<dyn LlmClient>) -> Agent {
    let registry = std::sync::Arc::new(ToolRegistry::new(
        vec![],
        std::sync::Arc::new(TracingToolLogger),
    ));
    Agent::new(
        AgentConfig {
            name: "test".to_string(),
            ..Default::default()
        },
        llm,
        std::sync::Arc::new(Mutex::new(ToolManager::new(registry))),
        None,
        std::sync::Arc::new(ChatClient::new("http://localhost:1")),
        None,
        None,
    )
}

fn judge_agent(
    verdict: &'static str,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
) -> Agent {
    agent_with_llm(std::sync::Arc::new(JudgeLlm { verdict, seen }))
}

/// LLM that returns an empty response; used where a real client is irrelevant.
struct NoopLlm;
#[async_trait::async_trait]
impl LlmClient for NoopLlm {
    async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
        Ok(String::new())
    }
    async fn stream(
        &self,
        _messages: &[Message],
        _chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        Ok(String::new())
    }
}
