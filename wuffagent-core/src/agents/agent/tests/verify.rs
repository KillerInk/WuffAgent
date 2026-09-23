use super::*;

#[tokio::test]
async fn test_verify_no_tool_outputs_skips_llm() {
    let agent = agent_with_llm(std::sync::Arc::new(RefuseLlm));
    let messages = vec![test_msg("user", "hello"), test_msg("assistant", "hi there")];
    let result = agent
        .verify_tool_outputs(&messages, "hello", "hi there", &CancellationToken::new())
        .await;
    let verdict = result.expect("no tool outputs -> auto-verified without an LLM call");
    assert!(verdict.verified, "no tool outputs -> auto-verified");
    assert!(
        verdict.judge_reason.is_empty(),
        "no judge call -> no reason text"
    );
}

#[tokio::test]
async fn test_verify_verdict_verified() {
    let agent = judge_agent("VERIFIED", std::sync::Arc::new(Mutex::new(Vec::new())));
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt\nb.txt"),
        test_msg("assistant", "There are two files: a.txt and b.txt."),
    ];
    let verdict = agent
        .verify_tool_outputs(
            &messages,
            "list the directory",
            "There are two files: a.txt and b.txt.",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(verdict.verified);
    assert_eq!(verdict.judge_reason, "VERIFIED");
}

#[tokio::test]
async fn test_verify_verdict_needs_fix() {
    let agent = judge_agent(
        "NEEDS_FIX: the response misses b.txt",
        std::sync::Arc::new(Mutex::new(Vec::new())),
    );
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt\nb.txt"),
        test_msg("assistant", "There is one file: a.txt."),
    ];
    let verdict = agent
        .verify_tool_outputs(
            &messages,
            "list the directory",
            "There is one file: a.txt.",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!verdict.verified);
    // S1: the judge's raw text must survive for the outcome memory.
    assert_eq!(verdict.judge_reason, "NEEDS_FIX: the response misses b.txt");
}

#[tokio::test]
async fn test_verify_ambiguous_verdict_defaults_to_verified() {
    let agent = judge_agent(
        "The answer looks plausible I guess",
        std::sync::Arc::new(Mutex::new(Vec::new())),
    );
    let messages = vec![
        test_msg("user", "list the directory"),
        test_msg("tool", "a.txt"),
        test_msg("assistant", "One file."),
    ];
    let verdict = agent
        .verify_tool_outputs(
            &messages,
            "list the directory",
            "One file.",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        verdict.verified,
        "ambiguous judge text defaults to verified"
    );
    assert_eq!(verdict.judge_reason, "The answer looks plausible I guess");
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
        .verify_tool_outputs(
            &messages,
            "read the readme",
            "The readme says hello world.",
            &CancellationToken::new(),
        )
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
            conv.push(test_msg(
                "assistant",
                &format!("answer {i} {}", "x".repeat(300)),
            ));
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
    agent.trimming.trim_messages(
        &mut messages,
        agent.client.trim_target_chars(),
        &agent.config.trim_config,
    );
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
    assert_eq!(
        agent.extract_original_request(&messages),
        VERIFICATION_NUDGE
    );

    // The post-fix call: the request captured at loop start is passed
    // through, and the judge prompt must contain it (and not the nudge).
    let original_request = "list the directory".to_string();
    let verdict = agent
        .verify_tool_outputs(
            &messages,
            &original_request,
            "There are two files: a.txt and b.txt.",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(verdict.verified);
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
