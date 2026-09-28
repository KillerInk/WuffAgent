//! `run_eval`: the eval harness (2b) — run a profile's saved golden/regression
//! evals headlessly, grade each against its `expect` criteria, and record an
//! `Eval` metrics line per run.
//!
//! Each eval runs on a FRESH, ISOLATED headless agent so it never touches the
//! live session:
//! - a fresh `ChatClient` (`session_client.fresh()`) gives an empty
//!   conversation store, so the eval's messages live only in that store;
//! - the shared `LlmClient` transport does the LLM calls;
//! - the shared `ToolManager` gives the eval agent the same tools a live turn
//!   has (`AgentBuilder::build` derives a per-agent view, so the eval agent
//!   locks its own tool-manager mutex, not the app's);
//! - `metrics_enabled = false` keeps the synthetic run out of the profile's
//!   real-run metrics — instead an `Eval` line (pass/fail + cost) is written.
//!
//! After the run, the existing verification judge (`verify_tool_outputs`)
//! grades the final response against the eval's `expect`. A per-eval timeout
//! bounds a wedged run; the outcome is reported as a pass/fail table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::agents::agent::AgentBuilder;
use crate::agents::metrics::MetricsLog;
use crate::agents::{AgentConfig, AgentManager, LlmClient};
use crate::client::ChatClient;
use crate::memory::evals::EvalStore;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolSchema, ToolResult,
};
use crate::tools::ToolManager;

/// Per-eval wall-clock budget (a backstop against a wedged LLM/tool run; the
/// profile's own `task_timeout_ms` is disabled for eval runs).
const EVAL_TIMEOUT_SECS: u64 = 180;

/// Cached current-thread tokio runtime for blocking on one headless eval from a
/// sync tool caller. Reuses the ambient runtime handle when available (the tool
/// normally runs inside `spawn_blocking`), otherwise this backstop (tests /
/// plain threads). Same pattern as `memory::manager`'s blocking helper.
static EVAL_RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build eval runtime")
});

/// Run an async future to completion (ambient runtime first, backstop second).
fn block_on_eval<F>(fut: F) -> F::Output
where
    F: std::future::Future,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => EVAL_RUNTIME.block_on(fut),
    }
}

/// The measured outcome of one headless eval run (before the pass/fail table).
struct EvalOnce {
    /// The verification judge's verdict against the eval's `expect`.
    verified: bool,
    /// The judge's raw reason (empty for the no-tool-outputs shortcut).
    judge_reason: String,
    /// Wall-clock duration of the run, in milliseconds.
    duration_ms: u64,
    /// Prompt tokens consumed by the run (0 when the server reports none).
    tokens_in: u64,
    /// Completion tokens produced by the run (0 when the server reports none).
    tokens_out: u64,
    /// Total tool calls executed (post-hoc `max_tool_calls` enforcement).
    tool_calls: usize,
}

/// Run ONE eval headlessly: build a fresh agent → execute the task → judge the
/// result against `expect`. Never panics; errors surface as `Err(String)`.
async fn run_eval_once(
    config: AgentConfig,
    llm_client: Arc<dyn LlmClient>,
    client: Arc<ChatClient>,
    tool_manager: Arc<Mutex<ToolManager>>,
    task: &str,
    expect: &str,
) -> Result<EvalOnce, String> {
    // Headless policy: no handoff/restart/hand_back/session-note (those drive
    // UI events / session swaps), no per-run metrics line (this is a synthetic
    // run — an `Eval` line is written instead), and the profile's own task
    // timeout disabled (the per-eval timeout is the single bound).
    let mut cfg = config;
    cfg.handoff_enabled = false;
    cfg.restart_enabled = false;
    cfg.hand_back_enabled = false;
    cfg.session_note_enabled = false;
    cfg.metrics_enabled = false;
    cfg.task_timeout_ms = 0;

    let mut agent = AgentBuilder::new(cfg, llm_client, client)
        .tool_manager(tool_manager)
        .build();

    let cancel = CancellationToken::new();
    let started = std::time::Instant::now();
    let response = agent
        .execute(task, None, &cancel)
        .await
        .map_err(|e| format!("eval run failed: {e}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    // Grade the final response against the eval's `expect` using the existing
    // verification judge. The fresh client holds exactly this eval's
    // conversation (one user message — the task — plus the run), so the whole
    // run is "this turn"; when the run made no tool calls the judge
    // short-circuits to `verified`.
    let verdict = agent
        .verify_tool_outputs(agent.messages(), expect, &response, &cancel)
        .await
        .map_err(|e| format!("verification failed: {e}"))?;

    let (tokens_in, tokens_out) = agent.run_tokens();
    let tool_calls = agent.run_stats().tool_calls;

    Ok(EvalOnce {
        verified: verdict.verified,
        judge_reason: verdict.judge_reason,
        duration_ms,
        tokens_in,
        tokens_out,
        tool_calls,
    })
}

/// Truncate a string to `max` chars (for the pass/fail table's reason column).
fn truncate(s: &str, max: usize) -> String {
    let mut t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        t.push('…');
    }
    t
}

/// Tool that runs a profile's saved evals headlessly and reports a pass/fail
/// table, recording an `Eval` metrics line per run (2b).
pub struct RunEvalTool {
    evals: Arc<EvalStore>,
    agents: Arc<AgentManager>,
    llm_client: Arc<dyn LlmClient>,
    /// The live session client — `fresh()` clones its connection settings for
    /// an isolated conversation store per eval run.
    session_client: Arc<ChatClient>,
    /// The app's tool manager (a per-agent view is derived at build time).
    tool_manager: Arc<Mutex<ToolManager>>,
}

impl RunEvalTool {
    pub fn new(
        evals: Arc<EvalStore>,
        agents: Arc<AgentManager>,
        llm_client: Arc<dyn LlmClient>,
        session_client: Arc<ChatClient>,
        tool_manager: Arc<Mutex<ToolManager>>,
    ) -> Self {
        Self {
            evals,
            agents,
            llm_client,
            session_client,
            tool_manager,
        }
    }
}

impl Tool for RunEvalTool {
    fn name(&self) -> &str {
        "run_eval"
    }

    fn description(&self) -> &str {
        "Run a profile's saved golden/regression evals headlessly: each eval's task \
         is given to a fresh, isolated agent (the same tools a live turn has), the \
         result is graded against the eval's `expect` criteria by the verification \
         judge, and an `Eval` metrics line (pass/fail + cost) is recorded. Params: \
         agent (profile name; required), eval_id (run just this eval), all (run \
         every saved eval; default true when eval_id is omitted)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "run_eval".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "agent".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Name of the agent profile to run the evals for (required)"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "eval_id".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Run only this eval (by id); omit to run all of the \
                                          profile's saved evals"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                    (
                        "all".to_string(),
                        FieldSchema {
                            type_name: "boolean".to_string(),
                            description: "When eval_id is omitted, run all saved evals (default: true)"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                ])),
                required: vec!["agent".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        // 1. Resolve the profile (a "chat" profile is synthetic, like run_self_improvement).
        let agent_name = match params.get::<String>("agent") {
            Some(n) if !n.is_empty() => n,
            _ => {
                let available = self.available_profiles();
                return Ok(ToolOutput::error(format!(
                    "agent is required (profile name). Available profiles: {available}"
                )));
            }
        };
        let profile = match self.agents.get_agent(&agent_name) {
            Some(c) => c,
            None if agent_name.eq_ignore_ascii_case(crate::agents::improvement::CHAT_PROFILE_NAME) => {
                crate::agents::improvement::synthetic_chat_config()
            }
            None => {
                let available = self.available_profiles();
                return Ok(ToolOutput::error(format!(
                    "agent profile '{agent_name}' not found. Available profiles: {available}"
                )));
            }
        };

        // 2. Select which evals to run: `eval_id` runs one; with no `eval_id`,
        // `all=true` (the default) runs every saved eval; `all=false` with no
        // `eval_id` is an explicit error (nothing was selected).
        let eval_id = params.get::<String>("eval_id").filter(|s| !s.is_empty());
        let all = params.get::<bool>("all").unwrap_or(true);
        let mut evals = self.evals.list(&agent_name);
        let saved_ids: Vec<String> = evals.iter().map(|e| e.id.clone()).collect();
        let hint = if saved_ids.is_empty() {
            "none (save one with save_eval first)".to_string()
        } else {
            saved_ids.join(", ")
        };
        match (&eval_id, all) {
            (Some(id), _) => evals.retain(|e| &e.id == id),
            (None, true) => {}
            (None, false) => {
                return Ok(ToolOutput::error(format!(
                    "all=false but no eval_id given — specify eval_id (or all=true) to \
                     select which eval(s) to run. Saved evals for '{agent_name}': {hint}"
                )));
            }
        }
        if evals.is_empty() {
            let what = eval_id
                .as_deref()
                .map(|id| format!("eval '{id}' not found"))
                .unwrap_or_else(|| "no evals saved".to_string());
            return Ok(ToolOutput::error(format!(
                "{what} for '{agent_name}'. Saved evals: {hint}"
            )));
        }

        // 3. Run each eval headlessly (bounded by the per-eval timeout).
        let log = MetricsLog::default();
        let mut rows: Vec<String> = Vec::with_capacity(evals.len());
        let mut passed = 0usize;
        let mut failed = 0usize;

        for eval in &evals {
            let started = std::time::Instant::now();
            let fresh = Arc::new(self.session_client.fresh());
            // The `tokio::time::timeout` must be constructed AND polled inside a
            // runtime (its timer needs a time reactor), so wrap the whole thing
            // in an `async` block that runs on `block_on_eval`'s runtime.
            let outcome = block_on_eval(async {
                tokio::time::timeout(
                    Duration::from_secs(EVAL_TIMEOUT_SECS),
                    run_eval_once(
                        profile.clone(),
                        self.llm_client.clone(),
                        fresh,
                        self.tool_manager.clone(),
                        &eval.task,
                        &eval.expect,
                    ),
                )
                .await
            });
            let row = match outcome {
                Ok(Ok(r)) => {
                    let over_budget = eval
                        .max_tool_calls
                        .is_some_and(|max| r.tool_calls > max);
                    let p = r.verified && !over_budget;
                    if p {
                        passed += 1;
                    } else {
                        failed += 1;
                    }
                    log.log_eval(
                        &agent_name,
                        &eval.id,
                        p,
                        r.duration_ms,
                        r.tokens_in,
                        r.tokens_out,
                    );
                    let budget_note = if over_budget {
                        format!(
                            " | OVER BUDGET: {} > {} tool calls",
                            r.tool_calls,
                            eval.max_tool_calls.unwrap()
                        )
                    } else {
                        String::new()
                    };
                    format!(
                        "[{}] {} — {}{budget_note} | {:.1}s, {} tool calls, {} tok in / {} out \
                         | judge: {}",
                        eval.id,
                        if p { "PASS" } else { "FAIL" },
                        if r.verified { "verified" } else { "not verified" },
                        r.duration_ms as f64 / 1000.0,
                        r.tool_calls,
                        r.tokens_in,
                        r.tokens_out,
                        truncate(&r.judge_reason, 160),
                    )
                }
                Ok(Err(e)) => {
                    failed += 1;
                    log.log_eval(
                        &agent_name,
                        &eval.id,
                        false,
                        started.elapsed().as_millis() as u64,
                        0,
                        0,
                    );
                    format!("[{}] ERROR — {e}", eval.id)
                }
                Err(_) => {
                    failed += 1;
                    log.log_eval(
                        &agent_name,
                        &eval.id,
                        false,
                        EVAL_TIMEOUT_SECS * 1000,
                        0,
                        0,
                    );
                    format!("[{}] TIMEOUT (> {EVAL_TIMEOUT_SECS}s)", eval.id)
                }
            };
            rows.push(row);
        }

        // 4. Report the pass/fail table.
        Ok(ToolOutput::success(format!(
            "Ran {} eval(s) for '{agent_name}': {} passed, {} failed.\n\n{}",
            evals.len(),
            passed,
            failed,
            rows.join("\n")
        )))
    }
}

impl RunEvalTool {
    /// A comma-joined list of known profile names (for error messages).
    fn available_profiles(&self) -> String {
        let names: Vec<String> = self
            .agents
            .list_agents()
            .map(|a| a.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        if names.is_empty() {
            "(none)".to_string()
        } else {
            names.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::AgentConfig;
    use crate::client::ConnectionSettings;
    use crate::llm::LlmClient;
    use crate::memory::evals::{Eval, EvalStore};
    use crate::tools::registry::ToolRegistry;
    use crate::tools::types::TracingToolLogger;
    use crate::types::Message;
    use std::io::{Read, Write};
    use async_trait::async_trait;

    /// A mock LLM client for the verification-judge path (not exercised by the
    /// no-tool-calls evals, but the tool still needs a judge client): always
    /// returns a fixed string.
    struct MockLlm {
        response: String,
    }
    #[async_trait]
    impl LlmClient for MockLlm {
        async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
            Ok(self.response.clone())
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _handler: Box<dyn FnMut(String) + Send + Sync>,
        ) -> Result<String, String> {
            Ok(self.response.clone())
        }
    }

    /// A minimal mock LLM HTTP server: answers every request with a canned SSE
    /// stream (one content delta + `finish_reason: stop` + `[DONE]`) so the
    /// agent's MAIN streaming call succeeds without a real LLM. The agent's main
    /// call goes through its `ChatClient` (HTTP), so the judge-only `MockLlm`
    /// cannot cover it. Returns the bound address + the serving thread handle
    /// (keep the handle alive for the duration of the test).
    fn spawn_mock_sse(content: &str) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let quoted = serde_json::to_string(content).unwrap();
        let body = format!(
            "data: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{quoted}}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{}}}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                // Read until the end of the request headers; the JSON body can
                // be ignored (the mock answers every request identically).
                let mut buf = [0u8; 4096];
                let mut got = Vec::new();
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            got.extend_from_slice(&buf[..n]);
                            if got.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        (addr, handle)
    }

    /// A `ChatClient` pointed at a mock SSE server (main LLM calls succeed),
    /// plus the serving thread handle (keep it alive).
    fn mock_client(content: &str) -> (Arc<ChatClient>, std::thread::JoinHandle<()>) {
        let (addr, handle) = spawn_mock_sse(content);
        let url = format!("http://{addr}");
        let settings = ConnectionSettings::new(&url, None);
        (Arc::new(ChatClient::from_settings(settings)), handle)
    }

    fn empty_tool_manager() -> Arc<Mutex<ToolManager>> {
        Arc::new(Mutex::new(ToolManager::new(Arc::new(ToolRegistry::new(
            vec![],
            Arc::new(TracingToolLogger),
        )))))
    }

    fn outcome(res: ToolResult<ToolOutput>) -> (bool, String) {
        match res {
            Ok(ToolOutput::Success(v)) => (true, v.as_str().unwrap_or("").to_string()),
            Ok(ToolOutput::Error(e)) => (false, e),
            Err(e) => panic!("unexpected ToolError: {e}"),
        }
    }

    /// `run_eval_once`: a fresh agent runs the task against the mock LLM (no
    /// tool calls) and the no-tool-calls verification shortcut marks it verified.
    #[test]
    fn test_run_eval_once_returns_outcome() {
        let (client, _srv) = mock_client("HELLO");
        let llm: Arc<dyn LlmClient> = Arc::new(MockLlm {
            response: "HELLO".to_string(),
        });
        let r = block_on_eval(run_eval_once(
            AgentConfig {
                name: "coder".to_string(),
                ..Default::default()
            },
            llm,
            client,
            empty_tool_manager(),
            "Return the exact word: HELLO",
            "The response must contain HELLO",
        ))
        .expect("run_eval_once should succeed");
        assert!(r.verified, "no tool calls -> verified shortcut");
        assert_eq!(r.tool_calls, 0);
        assert!(r.duration_ms < EVAL_TIMEOUT_SECS * 1000);
    }

    /// The tool's full path: a saved eval runs headlessly and is reported as a
    /// pass/fail table row (the mock response makes no tool calls -> verified).
    #[test]
    fn test_run_eval_tool_reports_table() {
        let eval_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EvalStore::new(eval_dir.path().to_path_buf()));
        store
            .save(
                "coder",
                Eval {
                    id: "greet".to_string(),
                    task: "Return the exact word: HELLO".to_string(),
                    expect: "The response must contain HELLO".to_string(),
                    max_tool_calls: None,
                },
            )
            .unwrap();

        let agent_dir = tempfile::tempdir().unwrap();
        let agents = Arc::new(AgentManager::new(agent_dir.path().to_path_buf()));
        agents
            .add_agent(&AgentConfig {
                name: "coder".to_string(),
                ..Default::default()
            })
            .unwrap();

        let (client, _srv) = mock_client("HELLO");
        let llm: Arc<dyn LlmClient> = Arc::new(MockLlm {
            response: "HELLO".to_string(),
        });
        let tool = RunEvalTool::new(store, agents, llm, client, empty_tool_manager());

        let mut values = HashMap::new();
        values.insert("agent".to_string(), serde_json::json!("coder"));
        let (ok, msg) = outcome(tool.execute(ToolParams { values }));
        assert!(ok, "got: {msg}");
        assert!(msg.contains("Ran 1 eval(s) for 'coder'"), "got: {msg}");
        assert!(msg.contains("1 passed, 0 failed"), "got: {msg}");
        assert!(msg.contains("[greet] PASS"), "got: {msg}");
    }

    /// `run_eval` with no saved evals for the profile is an explicit error (not
    /// an empty run) — no LLM call is made, so a plain client is fine.
    #[test]
    fn test_run_eval_no_evals_is_error() {
        let eval_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EvalStore::new(eval_dir.path().to_path_buf()));
        let agent_dir = tempfile::tempdir().unwrap();
        let agents = Arc::new(AgentManager::new(agent_dir.path().to_path_buf()));
        agents
            .add_agent(&AgentConfig {
                name: "coder".to_string(),
                ..Default::default()
            })
            .unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(MockLlm {
            response: "x".to_string(),
        });
        let client = Arc::new(ChatClient::from_settings(ConnectionSettings::new(
            "http://127.0.0.1:1",
            None,
        )));
        let tool = RunEvalTool::new(store, agents, llm, client, empty_tool_manager());
        let mut values = HashMap::new();
        values.insert("agent".to_string(), serde_json::json!("coder"));
        let (ok, msg) = outcome(tool.execute(ToolParams { values }));
        assert!(!ok, "got: {msg}");
        assert!(msg.contains("no evals saved"), "got: {msg}");
    }
}
