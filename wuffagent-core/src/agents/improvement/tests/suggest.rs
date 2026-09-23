use super::*;

// ── I1/I2/I3: the suggest_improvements prompt, trajectory, and wider fields ──

/// I1: the extraction prompt carries the trajectory (tool calls, errors,
/// verification attempts, prompt length) and the F5 rejected-suggestion
/// history as explicit negative evidence.
#[tokio::test]
async fn test_improvement_prompt_includes_trajectory_and_rejections() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Always run cargo check after edits",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Improvement for 'coder' rejected by the user: shorten the prompt — do not re-suggest the same change.",
            "improvement-review",
            &["improvement-rejected", "agent:coder"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: "[]".to_string(),
        prompts: prompts.clone(),
    });
    let stats = crate::agents::RunStats {
        tool_calls: 5,
        tool_errors: 2,
        verification_attempts: 3,
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "fix the parser",
        "fixed it",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("Trajectory: 5 tool calls (2 errors), 3 verification attempt(s)"),
        "prompt: {}",
        prompt
    );
    // The current prompt length ("You are a coding agent. Be precise." = 35).
    assert!(
        prompt.contains("current system prompt 35 chars"),
        "prompt: {}",
        prompt
    );
    assert!(
        prompt.contains("rejected by the user: shorten the prompt"),
        "rejected history must be in the prompt: {}",
        prompt
    );
}

/// I3: every returned suggestion carries the deterministic evidence (the
/// trajectory line + the lesson count/excerpt the improver saw).
#[tokio::test]
async fn test_evidence_attached_to_suggestions() {
    let dir = tempdir().unwrap();
    let (manager, _prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "The shell tool needs an existing working directory",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: r#"[{"agent_name":"coder","prompt_change":"new prompt","rationale":"r"}]"#
            .to_string(),
        prompts: Arc::new(Mutex::new(Vec::new())),
    });
    let stats = crate::agents::RunStats {
        tool_calls: 1,
        tool_errors: 0,
        verification_attempts: 1,
    };
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &stats,
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(suggestions.len(), 1);
    assert!(
        !suggestions[0].evidence.is_empty(),
        "evidence must be attached"
    );
    assert!(
        suggestions[0].evidence[0].starts_with("Trajectory:"),
        "first evidence line is the trajectory: {:?}",
        suggestions[0].evidence
    );
    assert!(
        suggestions[0].evidence[1].contains("1 lesson(s)"),
        "evidence names the lesson count: {:?}",
        suggestions[0].evidence
    );
}

/// I2: the wider fields parse from LLM JSON when present, and default to
/// None when the LLM omits them (old-format responses still parse).
#[tokio::test]
async fn test_wider_fields_parse_and_default() {
    let dir = tempdir().unwrap();
    let (manager, _prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Timeouts keep failing on long builds",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let full_json = r#"[{
        "agent_name":"coder","prompt_change":"p","rationale":"r",
        "allowed_tools":["file_io","shell"],
        "reasoning_effort":"high",
        "shell_config":{"shell_enabled":true,"allowed_commands":["cargo.*"],"shell_type":"bash","shell_timeout_ms":30000},
        "handoff_targets":["reviewer"],
        "task_timeout_ms":120000,
        "new_agents":[]
    }]"#;
    let llm = Arc::new(CaptureLlm {
        response: full_json.to_string(),
        prompts: Arc::new(Mutex::new(Vec::new())),
    });
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &crate::agents::RunStats::default(),
        llm.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(suggestions.len(), 1);
    let s = &suggestions[0];
    assert_eq!(
        s.allowed_tools,
        Some(vec!["file_io".to_string(), "shell".to_string()])
    );
    assert_eq!(
        s.reasoning_effort,
        Some(crate::types::ReasoningEffort::High)
    );
    assert!(s.shell_config.is_some() && s.shell_config.as_ref().unwrap().shell_enabled);
    assert_eq!(s.handoff_targets, Some(vec!["reviewer".to_string()]));
    assert_eq!(s.task_timeout_ms, Some(120_000));

    // Old-format response (no new fields) → all None.
    let old_json =
        r#"[{"agent_name":"coder","prompt_change":"p","rationale":"r","new_agents":[]}]"#;
    let llm2 = Arc::new(CaptureLlm {
        response: old_json.to_string(),
        prompts: Arc::new(Mutex::new(Vec::new())),
    });
    let suggestions = suggest_improvements(
        &manager,
        &test_agent_config(),
        "task",
        "result",
        &crate::agents::RunStats::default(),
        llm2.as_ref(),
    )
    .await
    .unwrap();
    assert!(suggestions[0].allowed_tools.is_none());
    assert!(suggestions[0].reasoning_effort.is_none());
    assert!(suggestions[0].shell_config.is_none());
    assert!(suggestions[0].handoff_targets.is_none());
    assert!(suggestions[0].task_timeout_ms.is_none());
}

/// I1: the lessons section is capped — long lessons are clipped at 400 chars
/// and the total stays within the budget even for a busy lesson store.
#[test]
fn test_cap_lessons_respects_budget() {
    let long = "x".repeat(300);
    let lessons: Vec<String> = (0..50).map(|i| format!("{long} lesson{i}")).collect();
    let capped = cap_lessons(&lessons);
    assert!(
        capped.len() <= LESSON_CHAR_BUDGET,
        "capped length {} exceeds budget",
        capped.len()
    );
    // 40-char lessons × 50 = 2000 chars — well under budget, all kept.
    let small: Vec<String> = (0..50).map(|i| format!("lesson{}", i)).collect();
    assert_eq!(cap_lessons(&small).lines().count(), 50);

    // A single very long lesson is clipped with a trailing ellipsis.
    let one = vec!["y".repeat(1000)];
    let capped = cap_lessons(&one);
    assert!(capped.chars().count() <= 401, "{}", capped.chars().count());
    assert!(capped.ends_with('…'));
}
