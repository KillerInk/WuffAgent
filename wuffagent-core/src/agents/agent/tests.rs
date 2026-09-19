//! Unit tests for the `agent` module (see `super`).

use super::*;
use crate::tools::registry::ToolRegistry;
use crate::tools::types::TracingToolLogger;

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
        })
        .unwrap();
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(Arc::new(registry))));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    Agent::new(config, llm_client, tool_manager, None, client, None, None)
}

#[test]
fn test_disabled_shell_removed_from_agent_schema() {
    let agent = make_agent_with_shell(false);
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        !names.contains(&"shell".to_string()),
        "a disabled shell should not be advertised: {:?}",
        names
    );
}

#[test]
fn test_enabled_shell_present_in_agent_schema() {
    let agent = make_agent_with_shell(true);
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        names.contains(&"shell".to_string()),
        "an enabled shell should be advertised: {:?}",
        names
    );
}

/// Temp agents dir with an enabled `coder` (a valid handoff target).
fn handoff_agents_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wuffagent_test_agent_handoff_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("coder.json"),
        r#"{"name":"coder","system_prompt":"Code things."}"#,
    )
    .unwrap();
    dir
}

/// `tag` must be unique per test: the fixture dir is shared with any
/// concurrently running test that uses the same tag, and each test deletes
/// it on cleanup (parallel test runs would race otherwise).
fn make_agent_with_handoff(enabled: bool, targets: Vec<String>, tag: &str) -> (Agent, std::path::PathBuf) {
    let dir = handoff_agents_dir(tag);
    let mut config = AgentConfig {
        name: "planner".to_string(),
        ..Default::default()
    };
    config.handoff_enabled = enabled;
    config.handoff_targets = targets;
    config.agents_dir = dir.clone();
    let llm_client = Arc::new(NoopLlm);
    let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
    let client = Arc::new(ChatClient::new("http://localhost:1"));
    let agent = Agent::new(config, llm_client, tool_manager, None, client, None, None);
    (agent, dir)
}

#[test]
fn test_handoff_tool_injected_when_enabled() {
    let (agent, dir) = make_agent_with_handoff(true, vec!["coder".to_string()], "inject_on");
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        names.contains(&"handoff".to_string()),
        "an enabled handoff should be advertised: {:?}",
        names
    );
    assert!(
        agent.handoff_mailbox.is_some(),
        "an enabled handoff should create a mailbox"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handoff_tool_absent_when_disabled() {
    let (agent, dir) = make_agent_with_handoff(false, vec!["coder".to_string()], "inject_off");
    let defs = agent.tool_manager.lock().unwrap().get_tool_definitions();
    let names: Vec<String> = defs.iter().map(|d| d.function.name.clone()).collect();
    assert!(
        !names.contains(&"handoff".to_string()),
        "a disabled handoff should not be advertised: {:?}",
        names
    );
    assert!(
        agent.handoff_mailbox.is_none(),
        "a disabled handoff should not create a mailbox"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_take_pending_handoff() {
    use crate::tools::types::ToolParams;

    let (agent, dir) = make_agent_with_handoff(true, vec!["coder".to_string()], "take_on");
    // Invoke the injected per-execution handoff tool through the manager.
    let params = ToolParams {
        values: serde_json::to_value(serde_json::json!({
            "agent": "coder",
            "task": "Implement the plan.",
        }))
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    };
    let tm = agent.tool_manager.lock().unwrap();
    let result = tm.execute("handoff", params).await;
    assert!(
        result.is_ok(),
        "handoff tool call should succeed: {:?}",
        result.err()
    );
    drop(tm);

    // First take: the request; second take: empty.
    let req = agent
        .take_pending_handoff()
        .expect("pending handoff expected");
    assert_eq!(req.agent, "coder");
    assert_eq!(req.config.name, "coder");
    assert_eq!(req.task, "Implement the plan.");
    assert!(
        agent.take_pending_handoff().is_none(),
        "mailbox is consumed exactly once"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // An agent without handoff never has a mailbox.
    let (agent2, dir2) = make_agent_with_handoff(false, Vec::new(), "take_off");
    assert!(agent2.handoff_mailbox.is_none());
    assert!(agent2.take_pending_handoff().is_none());
    let _ = std::fs::remove_dir_all(&dir2);
}

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

#[test]
fn test_agent_creation() {
    let agent = make_agent("test");
    assert_eq!(agent.config.name, "test");
}

#[test]
fn test_agent_per_agent_reasoning_effort() {
    let llm_client = Arc::new(NoopLlm);
    let tool_registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let tool_manager = Arc::new(Mutex::new(ToolManager::new(tool_registry)));
    // Global client set to Medium.
    let global_client = Arc::new({
        let mut c = ChatClient::new("http://localhost:1");
        c.set_reasoning_effort(crate::types::ReasoningEffort::Medium);
        c
    });

    // Agent with High: gets its own client clone with High.
    let mut config = AgentConfig::default();
    config.name = "researcher".to_string();
    config.reasoning_effort = crate::types::ReasoningEffort::High;
    let agent = Agent::new(
        config,
        llm_client.clone(),
        tool_manager.clone(),
        None,
        global_client.clone(),
        None,
        None,
    );
    assert_eq!(agent.client.reasoning_effort(), crate::types::ReasoningEffort::High);
    assert!(!Arc::ptr_eq(&agent.client, &global_client));

    // Agent with Off: shares the global client (inherits Medium).
    let mut config = AgentConfig::default();
    config.name = "coder".to_string();
    config.reasoning_effort = crate::types::ReasoningEffort::Off;
    let agent = Agent::new(
        config,
        llm_client,
        tool_manager,
        None,
        global_client.clone(),
        None,
        None,
    );
    assert_eq!(agent.client.reasoning_effort(), crate::types::ReasoningEffort::Medium);
    assert!(Arc::ptr_eq(&agent.client, &global_client));
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

fn judge_agent(verdict: &'static str, seen: std::sync::Arc<std::sync::Mutex<Vec<Message>>>) -> Agent {
    agent_with_llm(std::sync::Arc::new(JudgeLlm { verdict, seen }))
}

#[tokio::test]
async fn test_verify_no_tool_outputs_skips_llm() {
    let agent = agent_with_llm(std::sync::Arc::new(RefuseLlm));
    let messages = vec![test_msg("user", "hello"), test_msg("assistant", "hi there")];
    let result = agent
        .verify_tool_outputs(&messages, "hello", "hi there", &CancellationToken::new())
        .await;
    assert_eq!(result, Ok(true), "no tool outputs -> auto-verified without an LLM call");
}

#[tokio::test]
async fn test_verify_verdict_verified() {
    let agent = judge_agent("VERIFIED", std::sync::Arc::new(Mutex::new(Vec::new())));
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt\nb.txt"),
        test_msg("assistant", "There are two files: a.txt and b.txt."),
    ];
    assert_eq!(
        agent
            .verify_tool_outputs(&messages, "list the directory", "There are two files: a.txt and b.txt.", &CancellationToken::new())
            .await,
        Ok(true)
    );
}

#[tokio::test]
async fn test_verify_verdict_needs_fix() {
    let agent = judge_agent("NEEDS_FIX: the response misses b.txt", std::sync::Arc::new(Mutex::new(Vec::new())));
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt\nb.txt"),
        test_msg("assistant", "There is one file: a.txt."),
    ];
    assert_eq!(
        agent
            .verify_tool_outputs(&messages, "list the directory", "There is one file: a.txt.", &CancellationToken::new())
            .await,
        Ok(false)
    );
}

#[tokio::test]
async fn test_verify_ambiguous_verdict_defaults_to_verified() {
    let agent = judge_agent("The answer looks plausible I guess", std::sync::Arc::new(Mutex::new(Vec::new())));
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt"),
        test_msg("assistant", "One file."),
    ];
    assert_eq!(
        agent
            .verify_tool_outputs(&messages, "list the directory", "One file.", &CancellationToken::new())
            .await,
        Ok(true)
    );
}

#[tokio::test]
async fn test_verify_prompt_includes_final_response_and_outputs() {
    let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
    let agent = judge_agent("VERIFIED", seen.clone());
    let messages = vec![
        test_msg("user", "read the readme"),
        test_msg("tool", "README CONTENTS HERE"),
        test_msg("assistant", "The readme says hello world."),
    ];
    agent
        .verify_tool_outputs(&messages, "read the readme", "The readme says hello world.", &CancellationToken::new())
        .await
        .unwrap();
    let joined: String = seen
        .lock()
        .unwrap()
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("The readme says hello world."),
        "judge prompt must include the assistant's final response: {}",
        joined
    );
    assert!(
        joined.contains("README CONTENTS HERE"),
        "judge prompt must still include the tool outputs: {}",
        joined
    );
}

/// Regression (T1): after a trim on the agent path, the shared store must
/// be reconciled so it stays bounded instead of growing with the session.
#[test]
fn test_store_stays_bounded_after_trim_reconciliation() {
    // A client with a known n_ctx so the trim budgets are concrete:
    // trigger = 90% of the window, target = 50% (chars via the
    // uncalibrated chars-per-token default).
    let client = Arc::new({
        let c = ChatClient::new("http://localhost:1");
        c.set_n_ctx(1000);
        c
    });
    let registry = Arc::new(ToolRegistry::new(vec![], Arc::new(TracingToolLogger)));
    let agent = Agent::new(
        AgentConfig {
            name: "test".to_string(),
            ..Default::default()
        },
        Arc::new(NoopLlm),
        Arc::new(Mutex::new(ToolManager::new(registry))),
        None,
        client,
        None,
        None,
    );

    // Fill the store with 30 turns of large replies, then the current
    // turn's user message, as `execute` would have recorded it.
    {
        let mut conv = agent.client.conversation().lock().unwrap();
        for i in 0..30 {
            conv.push(test_msg("user", &format!("turn {i} question")));
            conv.push(test_msg("assistant", &format!("answer {i} {}", "x".repeat(300))));
        }
        conv.push(test_msg("user", "the current request"));
    }
    let before = agent.client.conversation().lock().unwrap().len();
    assert_eq!(before, 61);

    // Reproduce run_llm_loop's pre-call trim + store reconciliation.
    let mut messages = agent.build_initial_messages("the current request");
    assert!(
        crate::trimming::message_char_count(&messages) > agent.client.trim_trigger_chars(),
        "test setup must exceed the trim trigger ({} > {} chars)",
        crate::trimming::message_char_count(&messages),
        agent.client.trim_trigger_chars()
    );
    agent
        .trimming
        .trim_messages(&mut messages, agent.client.trim_target_chars(), &agent.config.trim_config);
    agent.reconcile_store(&messages);

    let store = agent.client.conversation().lock().unwrap();
    assert!(
        store.len() < before,
        "reconciliation must shrink the store ({} -> {})",
        before,
        store.len()
    );
    // Invariants: the store never holds request-only entries, and the
    // current turn's user message survives as the last entry.
    assert!(!store.iter().any(|m| m.role == "system"));
    assert!(!store.iter().any(|m| m.content == VERIFICATION_NUDGE));
    assert_eq!(
        store.last().map(|m| m.content.as_str()),
        Some("the current request")
    );
}

/// Regression (T2): on the second verification attempt the nudge is the
/// last user message in the request list. run_llm_loop therefore captures
/// the original request once at loop start and passes it through — the
/// judge must grade against the real request, not the nudge text.
#[tokio::test]
async fn test_verify_judges_against_request_captured_before_nudge() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = judge_agent("VERIFIED", seen.clone());
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt\nb.txt"),
        test_msg("assistant", "There is one file: a.txt."),
        test_msg("user", VERIFICATION_NUDGE),
        test_msg("tool", "a.txt\nb.txt"),
        test_msg("assistant", "There are two files: a.txt and b.txt."),
    ];
    // The pre-fix extraction now returns the nudge, not the request —
    // which is exactly why the loop captures it before any nudge exists.
    assert_eq!(agent.extract_original_request(&messages), VERIFICATION_NUDGE);

    // The post-fix call: the request captured at loop start is passed
    // through, and the judge prompt must contain it (and not the nudge).
    let original_request = "list the directory".to_string();
    assert_eq!(
        agent
            .verify_tool_outputs(
                &messages,
                &original_request,
                "There are two files: a.txt and b.txt.",
                &CancellationToken::new()
            )
            .await,
        Ok(true)
    );
    let joined: String = seen
        .lock()
        .unwrap()
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("list the directory"),
        "judge prompt must contain the original request: {}",
        joined
    );
    assert!(
        !joined.contains(VERIFICATION_NUDGE),
        "judge prompt must not contain the nudge text: {}",
        joined
    );
}

/// The nudge is request-only, but a user message that merely repeats the
/// nudge text (with a real timestamp) must stay storable.
#[test]
fn test_is_storable_nudge_vs_user_typed_nudge() {
    // The verification nudge as the loop pushes it: empty timestamp.
    let nudge = Message {
        role: "user".to_string(),
        content: VERIFICATION_NUDGE.to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    };
    assert!(!Agent::is_storable(&nudge));

    // A human typing the same sentence: real timestamp, stays in the store.
    let typed = Message {
        role: "user".to_string(),
        content: VERIFICATION_NUDGE.to_string(),
        timestamp: crate::types::format_timestamp(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    };
    assert!(Agent::is_storable(&typed));
}
