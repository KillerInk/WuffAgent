//! Unit tests for the `improver` module (see `super`).

use super::*;
use crate::memory::{MemoryConfig, MemoryEntry, MemoryType};
use tempfile::tempdir;

/// Manager with an isolated temp memories dir (default would hit
/// ~/.wuffagent/memories and leak real entries into the tests).
fn fresh_manager() -> (MemoryManager, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    (MemoryManager::new(config).unwrap(), dir)
}

#[test]
fn test_suggestion_serialization() {
    let s = ImprovementSuggestion {
        agent_name: "coder".to_string(),
        prompt_change: Some("You are a coding agent.".to_string()),
        rationale: "Better clarity".to_string(),
        new_agents: vec![],
        allowed_tools: Some(vec!["file_io".to_string()]),
        reasoning_effort: Some(crate::types::ReasoningEffort::Medium),
        shell_config: None,
        handoff_targets: None,
        task_timeout_ms: Some(90_000),
        evidence: vec!["Trajectory: 1 tool calls (0 errors)".to_string()],
    };
    let json = serde_json::to_string(&s).unwrap();
    let parsed: ImprovementSuggestion = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.agent_name, "coder");
    assert_eq!(parsed.prompt_change, Some("You are a coding agent.".to_string()));
    // I2: the wider fields survive a round trip.
    assert_eq!(parsed.allowed_tools, Some(vec!["file_io".to_string()]));
    assert_eq!(parsed.reasoning_effort, Some(crate::types::ReasoningEffort::Medium));
    assert_eq!(parsed.task_timeout_ms, Some(90_000));
    assert_eq!(parsed.evidence.len(), 1);
}

#[test]
fn test_new_agent_proposal_serialization() {
    let prop = NewAgentProposal {
        name: "researcher".to_string(),
        description: "Searches web".to_string(),
        system_prompt: "You are a researcher.".to_string(),
        allowed_tools: vec!["web_search".to_string()],
    };
    let json = serde_json::to_string(&prop).unwrap();
    let parsed: NewAgentProposal = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "researcher");
    assert_eq!(parsed.allowed_tools, vec!["web_search"]);
}

#[test]
fn test_collect_lessons_prefers_task_relevant_search() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Use cargo check before running tests in the rust workspace",
            "agent",
            &[],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Fact,
            "An unrelated fact about rust crates and packaging",
            "agent",
            &[],
        ))
        .unwrap();

    let lessons = collect_lessons(&manager, "coder", "run the tests in the rust workspace");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("cargo check"));
}

#[test]
fn test_collect_lessons_falls_back_to_recent_when_no_match() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "The shell tool fails when the working directory does not exist",
            "agent",
            &["shell"],
        ))
        .unwrap();

    // No keyword overlap between the query and the stored lesson, so the
    // recent-lessons fallback must surface it instead of skipping the check.
    let lessons = collect_lessons(&manager, "coder", "completely unrelated topic zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("shell tool"));
}

#[test]
fn test_collect_lessons_empty_store() {
    let (manager, _dir) = fresh_manager();
    assert!(collect_lessons(&manager, "coder", "some task").is_empty());
}

/// S3: a lesson tagged `agent:<name>` is collected for that agent even when
/// the task text has no keyword overlap with it (the tag is the primary
/// per-agent signal).
#[test]
fn test_collect_lessons_tag_first() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Prefer streaming responses for large files to avoid timeouts",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    // No overlap between the tag-less lesson text and this task — only the
    // agent tag can surface it.
    let lessons = collect_lessons(&manager, "coder", "topic with no keyword overlap zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("streaming responses"));
}

/// S3: the tag path WINS over the text-search path — a tagged lesson for the
/// agent is returned and an untagged lesson that matches the task text is not
/// mixed in while the tag path has hits.
#[test]
fn test_collect_lessons_tag_wins_over_search() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Tagged lesson about workspace layout for the coder agent",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Untagged lesson mentioning workspace layout keywords",
            "agent",
            &[],
        ))
        .unwrap();

    // The task text strongly matches the UNTAGGED lesson, but the tagged one
    // must win (and be the only result).
    let lessons = collect_lessons(&manager, "coder", "workspace layout keywords");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("Tagged lesson"));
}

/// S3: tag filtering is per agent — a lesson tagged for the researcher is
/// not picked up by the coder's tag path, and the researcher's own
/// collect_lessons finds it via the tag alone (no task-text overlap needed).
#[test]
fn test_collect_lessons_tag_is_per_agent() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "Researcher workflow note on citation style",
            "agent",
            &["agent:researcher"],
        ))
        .unwrap();

    // The manager-level tag filter is exact per agent.
    assert_eq!(manager.get_by_tag("agent:researcher").len(), 1);
    assert!(manager.get_by_tag("agent:coder").is_empty());

    let lessons = collect_lessons(&manager, "researcher", "unrelated topic zzz");
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].contains("citation style"));
}

// ── I1/I2/I3: the suggest_improvements prompt, trajectory, and wider fields ──

use std::sync::{Arc, Mutex};

/// Test double: captures the prompt it is given, returns a fixed response.
struct CaptureLlm {
    response: String,
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl LlmClient for CaptureLlm {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.prompts
            .lock()
            .unwrap()
            .push(messages.first().map(|m| m.content.clone()).unwrap_or_default());
        Ok(self.response.clone())
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let text = self.complete(messages).await?;
        chunk_handler(text.clone());
        Ok(text)
    }
}

/// Manager with auto_improve on (trigger lowered to 1 lesson).
fn auto_improve_manager(dir: &std::path::Path) -> (MemoryManager, Arc<Mutex<Vec<String>>>, String) {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let config = MemoryConfig {
        enabled: true,
        auto_improve: true,
        improvement_trigger_lessons: 1,
        memories_dir: Some(dir.to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    (manager, prompts, dir.to_str().unwrap().to_string())
}

fn test_agent_config() -> crate::agents::config::AgentConfig {
    let mut cfg = crate::agents::config::AgentConfig::default();
    cfg.name = "coder".to_string();
    cfg.description = "writes code".to_string();
    cfg.system_prompt = "You are a coding agent. Be precise.".to_string();
    cfg
}

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
    assert!(!suggestions[0].evidence.is_empty(), "evidence must be attached");
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
    assert_eq!(s.allowed_tools, Some(vec!["file_io".to_string(), "shell".to_string()]));
    assert_eq!(s.reasoning_effort, Some(crate::types::ReasoningEffort::High));
    assert!(s.shell_config.is_some() && s.shell_config.as_ref().unwrap().shell_enabled);
    assert_eq!(s.handoff_targets, Some(vec!["reviewer".to_string()]));
    assert_eq!(s.task_timeout_ms, Some(120_000));

    // Old-format response (no new fields) → all None.
    let old_json = r#"[{"agent_name":"coder","prompt_change":"p","rationale":"r","new_agents":[]}]"#;
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

// ── I4: cost control (evidence gate + state persistence) ──

/// Backdate an entry's timestamp (MemoryEntry::new always stamps `Utc::now()`).
fn backdate(entry: &mut MemoryEntry, days: i64) {
    entry.timestamp = Some(chrono::Utc::now() - chrono::Duration::days(days));
}

#[test]
fn test_evidence_gate_no_state_and_no_lessons() {
    let (manager, _dir) = fresh_manager();
    assert!(
        !manager.has_new_improvement_evidence(),
        "no state file + no lessons -> no evidence"
    );
}

#[test]
fn test_evidence_gate_no_state_with_lessons() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson that counts as evidence",
            "agent",
            &[],
        ))
        .unwrap();
    assert!(
        manager.has_new_improvement_evidence(),
        "no state file + a lesson -> evidence exists (first check baselines it)"
    );
}

#[test]
fn test_evidence_gate_ignores_non_lesson_entries() {
    let (manager, _dir) = fresh_manager();
    manager
        .add(MemoryEntry::new(MemoryType::Fact, "A fact is not evidence", "agent", &[]))
        .unwrap();
    assert!(
        !manager.has_new_improvement_evidence(),
        "Facts (e.g. I5 markers) are reference points, not evidence"
    );
}

#[test]
fn test_evidence_gate_old_lessons_then_new_lesson() {
    let (manager, _dir) = fresh_manager();
    // A lesson that predates the check.
    let mut old = MemoryEntry::new(MemoryType::Lesson, "An old lesson", "agent", &[]);
    backdate(&mut old, 1);
    manager.add(old).unwrap();

    manager.record_improvement_check();
    assert!(
        !manager.has_new_improvement_evidence(),
        "only pre-check lessons -> no NEW evidence"
    );

    // A new lesson after the check re-arms the gate.
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A brand new lesson after the check",
            "agent",
            &[],
        ))
        .unwrap();
    assert!(
        manager.has_new_improvement_evidence(),
        "post-check lesson -> evidence again"
    );
}

#[test]
fn test_improvement_state_persists_across_restart() {
    let dir = tempdir().unwrap();
    let make = || {
        let config = MemoryConfig {
            memories_dir: Some(dir.path().to_str().unwrap().to_string()),
            ..Default::default()
        };
        MemoryManager::new(config).unwrap()
    };

    let first = make();
    first
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A lesson before restart",
            "agent",
            &[],
        ))
        .unwrap();
    first.record_improvement_check();
    drop(first);

    // "Restart": a fresh manager on the same dir must see the recorded check,
    // so the pre-restart lesson is not re-counted as new evidence.
    let second = make();
    assert!(
        !second.has_new_improvement_evidence(),
        "state file must survive a restart"
    );
}

#[test]
fn test_improvement_state_corrupt_file_tolerated() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("improvement_state.json"), "not json at all").unwrap();

    let config = MemoryConfig {
        memories_dir: Some(dir.path().to_str().unwrap().to_string()),
        ..Default::default()
    };
    let manager = MemoryManager::new(config).unwrap();
    manager
        .add(MemoryEntry::new(MemoryType::Lesson, "A lesson", "agent", &[]))
        .unwrap();

    // Corrupt state -> treated as "no check recorded" -> lessons are evidence.
    assert!(manager.has_new_improvement_evidence());

    // Recording overwrites the corrupt file with valid JSON.
    manager.record_improvement_check();
    let content = std::fs::read_to_string(dir.path().join("improvement_state.json")).unwrap();
    assert!(content.contains("last_check"), "content: {}", content);
    assert!(
        serde_json::from_str::<serde_json::Value>(&content).is_ok(),
        "state file must be valid JSON after recording"
    );
}

// ── I5: effect check (approved-change marker + outcomes since) ──

/// Build an approved-change marker (Fact, `improvement-applied` +
/// `agent:<name>`), backdated `days_ago` days.
fn marker_for(agent: &str, days_ago: i64) -> MemoryEntry {
    let mut e = MemoryEntry::new(
        MemoryType::Fact,
        &format!("Prompt for agent '{}' changed via approved improvement.", agent),
        "improvement-review",
        &["improvement-applied", &format!("agent:{}", agent)],
    );
    backdate(&mut e, days_ago);
    e
}

#[tokio::test]
async fn test_effect_check_includes_outcomes_since_marker() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    // Marker: approved prompt change 3 days ago.
    manager.add(marker_for("coder", 3)).unwrap();
    // Outcomes since the marker (tagged for the agent).
    let mut o1 = MemoryEntry::new(
        MemoryType::Lesson,
        "Verification failed: tests still red after the change",
        "verification",
        &["agent:coder", "verification"],
    );
    backdate(&mut o1, 1);
    manager.add(o1).unwrap();
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "User feedback: result rated poorly",
            "user-feedback",
            &["agent:coder", "user-feedback"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm {
        response: r#"[{"agent_name": "coder", "prompt_change": "p", "rationale": "r"}]"#.to_string(),
        prompts: prompts.clone(),
    });
    let stats = RunStats { tool_calls: 1, tool_errors: 0, verification_attempts: 0 };
    let suggestions =
        suggest_improvements(&manager, &test_agent_config(), "task", "result", &stats, llm.as_ref())
            .await
            .unwrap();

    assert_eq!(suggestions.len(), 1);
    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("was last changed via an approved improvement 3 day(s) ago"),
        "prompt: {}",
        prompt
    );
    assert!(
        prompt.contains("Outcomes recorded for this agent since that change:"),
        "prompt: {}",
        prompt
    );
    assert!(prompt.contains("tests still red after the change"), "prompt: {}", prompt);
    assert!(prompt.contains("result rated poorly"), "prompt: {}", prompt);
    assert!(prompt.contains("you may propose reverting the prompt"), "prompt: {}", prompt);
    // The effect-check input is attached as deterministic evidence.
    assert!(
        suggestions[0].evidence.iter().any(|e| e.starts_with("Effect check:")),
        "evidence: {:?}",
        suggestions[0].evidence
    );
}

#[tokio::test]
async fn test_effect_check_absent_without_marker() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());
    manager
        .add(MemoryEntry::new(
            MemoryType::Lesson,
            "A plain lesson",
            "agent",
            &["agent:coder"],
        ))
        .unwrap();

    let llm = Arc::new(CaptureLlm { response: "[]".to_string(), prompts: prompts.clone() });
    let stats = RunStats { tool_calls: 1, tool_errors: 0, verification_attempts: 0 };
    let suggestions =
        suggest_improvements(&manager, &test_agent_config(), "task", "result", &stats, llm.as_ref())
            .await
            .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        !prompt.contains("was last changed via an approved improvement"),
        "no marker -> no effect-check section; prompt: {}",
        prompt
    );
    assert!(
        !prompt.contains("Outcomes recorded for this agent since that change:"),
        "prompt: {}",
        prompt
    );
}

#[tokio::test]
async fn test_effect_check_marker_without_outcomes_shows_none_yet() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    manager.add(marker_for("coder", 3)).unwrap();
    // A trigger lesson OLDER than the marker: it still triggers the check
    // (tag search has no timestamp filter) but is not an outcome "since".
    let mut old = MemoryEntry::new(
        MemoryType::Lesson,
        "A pre-change lesson",
        "agent",
        &["agent:coder"],
    );
    backdate(&mut old, 5);
    manager.add(old).unwrap();

    let llm = Arc::new(CaptureLlm { response: "[]".to_string(), prompts: prompts.clone() });
    let stats = RunStats { tool_calls: 1, tool_errors: 0, verification_attempts: 0 };
    let suggestions =
        suggest_improvements(&manager, &test_agent_config(), "task", "result", &stats, llm.as_ref())
            .await
            .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains("Outcomes recorded for this agent since that change:"),
        "section must exist when a marker does; prompt: {}",
        prompt
    );
    assert!(prompt.contains("(none yet)"), "prompt: {}", prompt);
}

#[tokio::test]
async fn test_effect_check_excludes_outcomes_older_than_marker() {
    let dir = tempdir().unwrap();
    let (manager, prompts, _keep) = auto_improve_manager(dir.path());

    manager.add(marker_for("coder", 3)).unwrap();
    // A trigger lesson and an outcome, both OLDER than the marker.
    let mut lesson = MemoryEntry::new(
        MemoryType::Lesson,
        "Trigger lesson from before the change",
        "agent",
        &["agent:coder"],
    );
    backdate(&mut lesson, 5);
    manager.add(lesson).unwrap();
    let mut old_outcome = MemoryEntry::new(
        MemoryType::Lesson,
        "Stale outcome from before the change",
        "verification",
        &["agent:coder", "verification"],
    );
    backdate(&mut old_outcome, 4);
    manager.add(old_outcome).unwrap();

    let llm = Arc::new(CaptureLlm { response: "[]".to_string(), prompts: prompts.clone() });
    let stats = RunStats { tool_calls: 1, tool_errors: 0, verification_attempts: 0 };
    let suggestions =
        suggest_improvements(&manager, &test_agent_config(), "task", "result", &stats, llm.as_ref())
            .await
            .unwrap();
    assert!(suggestions.is_empty());

    let prompt = &prompts.lock().unwrap()[0];
    // The section exists (marker present) but lists no outcomes — the stale
    // outcome is older than the marker (it still appears in the lessons
    // section, which has no timestamp filter).
    assert!(
        prompt.contains("Outcomes recorded for this agent since that change:"),
        "prompt: {}",
        prompt
    );
    assert!(prompt.contains("(none yet)"), "prompt: {}", prompt);
}
