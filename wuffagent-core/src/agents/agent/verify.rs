//! Output verification: judge call + outcome persistence + the loop's
//! verify-&-complete section (extracted A5).
//! Split out of agents/agent.rs (A1).

use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::r#loop::{MAX_VERIFICATION_ATTEMPTS, RunOutcome};
use super::truncate_chars;
use super::Agent;
use crate::memory::{MemoryEntry, MemoryManager, MemoryType};
use crate::types::{Message, Usage};

/// Maximum characters of tool output to include in summary.
const TOOL_OUTPUT_SUMMARY_CHARS: usize = 200;
/// Maximum characters of user request for verification prompts.
const REQUEST_TRUNCATION_CHARS: usize = 500;
/// Maximum characters of the assistant's final response for verification
/// prompts. The response is model-generated (not user-controlled), so a
/// generous budget is safe — this only bounds the judge call's prompt size.
const RESPONSE_TRUNCATION_CHARS: usize = 2000;
/// S1: max characters of the judge's reason / the task snippet stored in a
/// verification-outcome lesson memory (keeps the entry compact).
const VERIFICATION_OUTCOME_REASON_CHARS: usize = 300;
const VERIFICATION_OUTCOME_TASK_CHARS: usize = 200;
/// S1: persist a verification outcome that did NOT pass on the first try.
///
/// `verdict` is `"verified_after_retry"` (the judge failed the first attempt,
/// the nudged retry passed) or `"gave_up"` (the nudge loop was exhausted).
/// Stored as a `lesson` memory (tags `agent:<name>` + `verification`, source
/// `verification`) through the shared save path — the dedup gate collapses
/// repeated identical outcomes. Returns `Ok(true)` when an entry was saved,
/// `Ok(false)` when skipped (memory disabled), `Err` on store failure.
pub fn record_verification_outcome(
    memory: &MemoryManager,
    agent_name: &str,
    verdict: &str,
    attempts: u32,
    judge_reason: &str,
    task: &str,
) -> Result<bool, String> {
    if !memory.config().enabled {
        return Ok(false);
    }
    let agent_tag = format!("agent:{agent_name}");
    let reason = if judge_reason.trim().is_empty() {
        "(no reason given)".to_string()
    } else {
        truncate_chars(judge_reason, VERIFICATION_OUTCOME_REASON_CHARS)
    };
    let entry = MemoryEntry::new(
        MemoryType::Lesson,
        &format!(
            "Verification outcome for agent '{}': {} after {} verification attempt(s). Judge: {}. Task: {}",
            agent_name,
            verdict,
            attempts,
            reason,
            truncate_chars(task, VERIFICATION_OUTCOME_TASK_CHARS)
        ),
        "verification",
        &[agent_tag.as_str(), "verification"],
    );
    memory.add(entry).map(|_| true)
}
/// System prompt for response verification.
///
/// The judge grades the assistant's *response* against the tool outputs it
/// relied on — not the raw tool outputs alone. The old wording ("do the tool
/// outputs answer the request?") failed legitimate answers: intermediate
/// outputs (file dumps, search results) rarely contain the full answer by
/// themselves, so the judge returned NEEDS_FIX and the loop wasted an extra
/// LLM round re-asking a model that had already answered correctly.
static VERIFICATION_SYSTEM_PROMPT: &str =
    "You are verifying whether an assistant's response fully satisfies the user's request, \
     using the tool outputs it relied on as evidence. \
     Respond with exactly 'VERIFIED' if the response is correct, complete, and consistent with the tool outputs. \
     Respond with 'NEEDS_FIX' followed by a brief explanation ONLY if the response is factually wrong, \
     incomplete, or contradicts the tool outputs. \
     Do NOT reply NEEDS_FIX merely because the tool outputs alone do not spell out the full answer — \
     the response itself is what you are grading. \
     On the LAST line of your reply, write 'SCORE: <x>' where x is a decimal between 0.0 and 1.0 \
     for how well the response satisfies the request (1.0 = fully satisfied).";
/// The verification judge's verdict on the assistant's final response for
/// the current turn (returned by `verify_tool_outputs`).
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationVerdict {
    /// Whether the judge (or the no-tool-outputs shortcut) accepts the
    /// response.
    pub verified: bool,
    /// The judge's raw response text — empty for the no-tool-outputs
    /// shortcut. Kept for logging and as S1 outcome evidence.
    pub judge_reason: String,
    /// 1d: wall-clock ms the judge LLM call took (0 for the no-tool-outputs
    /// shortcut) — accumulated into `VerificationState::judge_ms` by the
    /// caller and folded into the run's `llm_ms` bucket.
    pub judge_ms: u64,
    /// 1c: the judge's 0..=1 quality score from its `SCORE: <x>` line
    /// (`Some(1.0)` for the no-tool-outputs shortcut; None when the judge
    /// line is absent or malformed). Eval lines record it; run lines don't.
    pub score: Option<f64>,
}

/// 1c: parse the judge's `SCORE: <x>` line into a clamped 0..=1 score.
///
/// Case-insensitive; takes the text on the line containing the FIRST
/// `SCORE:` and parses it as a float. `None` when the marker is absent,
/// the value is not a finite float, or parsing fails. Out-of-range values
/// are clamped to 0..=1 (a model that writes 1.5 for "great" still counts
/// as fully satisfied).
pub fn parse_judge_score(text: &str) -> Option<f64> {
    let upper = text.to_uppercase();
    let start = upper.find("SCORE:")? + "SCORE:".len();
    let rest = &text[start..];
    let line = &rest[..rest.find('\n').unwrap_or(rest.len())];
    // First whitespace-separated token (tolerates trailing commentary).
    let v: f64 = line.split_whitespace().next()?.parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod score_tests {
    use super::parse_judge_score;

    #[test]
    fn parses_valid_scores() {
        assert_eq!(parse_judge_score("VERIFIED\nSCORE: 0.9"), Some(0.9));
        assert_eq!(parse_judge_score("score: 0.25"), Some(0.25));
        assert_eq!(parse_judge_score("SCORE:1.0"), Some(1.0));
        assert_eq!(parse_judge_score("SCORE: 0"), Some(0.0));
    }

    #[test]
    fn clamps_out_of_range() {
        assert_eq!(parse_judge_score("SCORE: 1.5"), Some(1.0));
        assert_eq!(parse_judge_score("SCORE: -0.2"), Some(0.0));
    }

    #[test]
    fn missing_or_malformed_is_none() {
        assert_eq!(parse_judge_score("VERIFIED"), None);
        assert_eq!(parse_judge_score("SCORE: abc"), None);
        assert_eq!(parse_judge_score("SCORE:"), None);
        assert_eq!(parse_judge_score("SCORE: 0.5 but wait"), Some(0.5));
    }
}

impl Agent {

    /// Verify that the assistant's final response satisfies the user's request.
    ///
    /// The judge LLM sees three things: the original user request (truncated,
    /// to blunt prompt injection), summaries of the tool outputs, and the
    /// assistant's final response — and it grades the RESPONSE against the
    /// outputs as evidence. (The pre-fix prompt asked only "do the tool
    /// outputs answer the request?", which failed correct answers:
    /// intermediate outputs rarely contain the full answer by themselves.)
    pub(crate) async fn verify_tool_outputs(
        &self,
        messages: &[Message],
        original_request: &str,
        final_response: &str,
        cancel_token: &CancellationToken,
    ) -> Result<VerificationVerdict, String> {
        // Scope the evidence to the current turn: only tool outputs produced
        // after the most recent user message count as evidence for this
        // turn's response. In a multi-turn session the full history holds stale
        // tool results from earlier turns; feeding those to the judge made it
        // return NEEDS_FIX for a perfectly complete answer to the current
        // request, which then re-asked the model after it had already finished.
        let turn_start = messages.iter().rposition(|m| m.role == "user").unwrap_or(0);
        let tool_outputs: Vec<String> = messages
            .iter()
            .skip(turn_start)
            .filter(|m| m.role == "tool")
            // Truncate on the &str: the full tool output (potentially 100KB+)
            // is never needed — only the summary prefix is.
            .map(|m| m.content.chars().take(TOOL_OUTPUT_SUMMARY_CHARS).collect())
            .collect();

        // Skip verification if no tool calls were made this turn.
        if tool_outputs.is_empty() {
            return Ok(VerificationVerdict {
                verified: true,
                judge_reason: String::new(),
                judge_ms: 0,
                score: Some(1.0),
            });
        }
        let recent_tool_summary: String = tool_outputs.join("\n");

        let verification_messages = vec![
            Message {
                role: "system".to_string(),
                content: VERIFICATION_SYSTEM_PROMPT.to_string(),
                timestamp: String::new(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                image: None,
            },
            Message {
                role: "user".to_string(),
                // Truncate the original request to avoid prompt injection
                // via oversized or adversarially crafted messages.
                content: format!(
                    "User request (truncated to {} chars):\n{}\n\nRecent tool outputs ({} chars each, truncated):\n{}\n\nAssistant response (truncated to {} chars):\n{}\n\nDoes the assistant response fully satisfy the user's request?",
                    REQUEST_TRUNCATION_CHARS,
                    &original_request.chars().take(REQUEST_TRUNCATION_CHARS).collect::<String>(),
                    TOOL_OUTPUT_SUMMARY_CHARS,
                    recent_tool_summary,
                    RESPONSE_TRUNCATION_CHARS,
                    &final_response.chars().take(RESPONSE_TRUNCATION_CHARS).collect::<String>()
                ),
                timestamp: crate::types::format_timestamp(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                image: None,
            },
        ];

        // Check cancellation before making the LLM call.
        if cancel_token.is_cancelled() {
            return Err("Verification cancelled".to_string());
        }

        // The judge call goes through the STREAMING path (like the main LLM
        // rounds), not the non-streaming one. The non-streaming client's
        // TOTAL request timeout (ChatClient::DEFAULT_TIMEOUT_SECS = 300 s)
        // used to fire here: on a slow local model a judge response can
        // legitimately take minutes to generate (e.g. a long thinking
        // block), so every session end stalled for 5 minutes and logged
        // "Verification LLM call failed: HTTP error: error sending request".
        // The streaming client has no total timeout — only a 300 s idle
        // read timeout that still bounds a truly hung server (no bytes
        // flowing for 5 min). An earlier fixed 60 s-per-attempt cap was
        // removed for the same slow-local-model reason.
        let judge_started = Instant::now();
        // Status bar: the judge is its own activity ("judge"). LabeledLlm
        // wraps the shared client; without a tracker (tests, headless) it
        // falls back to the plain client so mock-based tests keep working.
        let judge_llm = crate::activity::LabeledLlm::wrap(
            std::sync::Arc::clone(&self.llm_client),
            self.activity.clone(),
            "judge",
            Some(self.session_id()),
        );
        let judge_client: &dyn crate::llm::LlmClient = match &judge_llm {
            Some(l) => l.as_ref(),
            None => self.llm_client.as_ref(),
        };
        let cancel_clone = cancel_token.clone();
        let result = tokio::select! {
            result = judge_client.stream(&verification_messages, Box::new(|_chunk: String| {})) => result,
            _ = cancel_clone.cancelled() => Err("Verification cancelled".to_string()),
        };
        tracing::debug!(
            "[AGENT] Verification judge call finished in {:.1}s (ok={})",
            judge_started.elapsed().as_secs_f32(),
            result.is_ok()
        );
        let response = match result {
            Ok(r) => r,
            Err(e) => return Err(format!("Verification LLM call failed: {}", e)),
        };

        // Robust verification: check NEEDS_FIX first (takes precedence),
        // then check if the response is clearly affirmative.
        let response_upper = response.to_uppercase();
        if response_upper.contains("NEEDS_FIX")
            || response_upper.contains("NOT SATISFIED")
            || response_upper.contains("INCORRECT")
            || response_upper.contains("INCOMPLETE")
        {
            let score = parse_judge_score(&response);
            Ok(VerificationVerdict {
                verified: false,
                judge_reason: response,
                judge_ms: judge_started.elapsed().as_millis() as u64,
                score,
            })
        } else {
            // VERIFIED, or unclear — default to verified (better to continue
            // than to abort a successful execution on an ambiguous LLM
            // response). The judge text is kept either way for S1 evidence.
            let score = parse_judge_score(&response);
            Ok(VerificationVerdict {
                verified: true,
                judge_reason: response,
                judge_ms: judge_started.elapsed().as_millis() as u64,
                score,
            })
        }
    }

    /// S1: store a non-first-try verification outcome as a lesson memory.
    /// No-op when the agent has no memory manager; store failures are logged,
    /// never fatal to the run.
    pub(crate) fn store_verification_outcome(
        &self,
        verdict: &str,
        attempts: u32,
        judge_reason: &str,
        task: &str,
    ) {
        let Some(memory) = &self.memory else {
            return;
        };
        match record_verification_outcome(
            memory,
            &self.config.name,
            verdict,
            attempts,
            judge_reason,
            task,
        ) {
            Ok(true) => tracing::debug!(
                "[AGENT] Stored verification outcome for '{}' ({})",
                self.config.name,
                verdict
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!(
                "[AGENT] Failed to store verification outcome for '{}': {}",
                self.config.name,
                e
            ),
        }
    }
}

/// Mutable verification bookkeeping for one run: the attempt counter, the
/// last NEEDS_FIX reason seen (S1 outcome evidence), and the terminal
/// verification outcome (M1 metrics).
pub(crate) struct VerificationState {
    /// Number of verification attempts made this run.
    pub(crate) attempts: u32,
    /// The last NEEDS_FIX judge reason seen this run (S1 outcome evidence).
    pub(crate) last_failed_reason: String,
    /// M1: the terminal verification outcome, set when the run COMPLETES via
    /// the verification path (the attempt count alone is ambiguous: with
    /// MAX_VERIFICATION_ATTEMPTS=2 a second-attempt pass and a give-up both
    /// end at attempts==2). `None` = the run ended before completing
    /// verification (handoff/restart/hand-back).
    pub(crate) final_outcome: Option<crate::agents::metrics::RunOutcome>,
    /// 1d: cumulative wall-clock ms spent in verification judge calls this
    /// run (folded into the run's `llm_ms` bucket at run end).
    pub(crate) judge_ms: u64,
}

impl VerificationState {
    pub(crate) fn new() -> Self {
        Self {
            attempts: 0,
            last_failed_reason: String::new(),
            final_outcome: None,
            judge_ms: 0,
        }
    }
}

impl Agent {
    /// Run the loop's final section for a round with no tool calls: judge
    /// the response against the tool outputs it relied on, either nudge a
    /// retry (NEEDS_FIX within budget → `Ok(None)`) or complete the turn
    /// (StreamComplete + `RunOutcome::Completed` → `Ok(Some(outcome))`),
    /// persisting S1 verification-outcome lessons where applicable.
    pub(crate) async fn verify_or_complete(
        &self,
        messages: &mut Vec<Message>,
        display_content: String,
        usage: Option<Usage>,
        original_request: &str,
        cancel_token: &CancellationToken,
        state: &mut VerificationState,
    ) -> Result<Option<RunOutcome>, String> {
        // ── No more tool calls ──────────────────────────────────────
        if state.attempts >= MAX_VERIFICATION_ATTEMPTS {
            // S1: the nudge loop was exhausted without a passing verdict —
            // record it as negative evidence.
            self.store_verification_outcome(
                "gave_up",
                state.attempts,
                &state.last_failed_reason,
                original_request,
            );
            // M1: terminal outcome for the metrics line.
            state.final_outcome = Some(crate::agents::metrics::RunOutcome::GaveUp);
            tracing::info!(
                "[AGENT] Agent '{}' completed ({} verification attempts)",
                self.config.name,
                state.attempts
            );
            self.send_event(crate::types::AppEvent::StreamComplete {
                content: display_content.clone(),
                usage,
                session_id: self.session_id(),
            });
            return Ok(Some(RunOutcome::Completed(display_content)));
        }
        state.attempts += 1;

        let verification_result = self
            .verify_tool_outputs(messages, original_request, &display_content, cancel_token)
            .await;
        // 1d: accumulate the judge's wall-clock into the run's LLM bucket.
        if let Ok(v) = &verification_result {
            state.judge_ms += v.judge_ms;
        }
        match verification_result {
            Ok(verdict) if verdict.verified => {
                // S1: a pass that needed a retry is negative evidence for
                // the first attempt (a first-try pass stays unrecorded —
                // no noise).
                if state.attempts > 1 {
                    self.store_verification_outcome(
                        "verified_after_retry",
                        state.attempts,
                        &state.last_failed_reason,
                        original_request,
                    );
                }
                // M1: terminal outcome for the metrics line.
                state.final_outcome = Some(if state.attempts > 1 {
                    crate::agents::metrics::RunOutcome::VerifiedAfterRetry
                } else {
                    crate::agents::metrics::RunOutcome::Verified
                });
                tracing::info!(
                    "[AGENT] Agent '{}' completed (verified)",
                    self.config.name
                );
                self.send_event(crate::types::AppEvent::StreamComplete {
                    content: display_content.clone(),
                    usage,
                    session_id: self.session_id(),
                });
                Ok(Some(RunOutcome::Completed(display_content)))
            }
            Ok(verdict) => {
                if !verdict.judge_reason.trim().is_empty() {
                    state.last_failed_reason = verdict.judge_reason.clone();
                }
                tracing::warn!(
                    "[AGENT] Agent '{}' verification failed (attempt {}/{}): {}, feeding feedback to LLM",
                    self.config.name,
                    state.attempts,
                    MAX_VERIFICATION_ATTEMPTS,
                    truncate_chars(&verdict.judge_reason, 200)
                );
                messages.push(Message {
                    role: "user".to_string(),
                    content: super::VERIFICATION_NUDGE.to_string(),
                    timestamp: String::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    image: None,
                });
                Ok(None)
            }
            Err(e) => {
                tracing::warn!(
                    "[AGENT] Agent '{}' verification error: {}, proceeding with response",
                    self.config.name,
                    e
                );
                // M1: verification failed to run, but the response is accepted
                // (default-pass) — record it as verified.
                state.final_outcome = Some(crate::agents::metrics::RunOutcome::Verified);
                self.send_event(crate::types::AppEvent::StreamComplete {
                    content: display_content.clone(),
                    usage,
                    session_id: self.session_id(),
                });
                Ok(Some(RunOutcome::Completed(display_content)))
            }
        }
    }
}
