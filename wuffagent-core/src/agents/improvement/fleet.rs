//! The fleet-wide (cross-agent) review: the compact evidence-JSON builder,
//! the suggestion call/parse, and the roster name sanitisation (Dice
//! coefficient re-targeting onto a real profile).

use crate::llm::LlmClient;
use crate::memory::manager::MemoryManager;
use crate::memory::types::MemoryEntry;
use crate::types::{ImprovementSuggestion, Message};

use super::{CHAT_PROFILE_NAME, truncate_for_evidence};

/// 2d: character budget for the fleet evidence block (~2k tokens — the
/// plan's cap for the cross-agent summary handed to the fleet improver).
const FLEET_EVIDENCE_CHAR_BUDGET: usize = 8_000;
/// 2d: per-agent cap on top-lesson excerpts in the fleet evidence block.
const FLEET_TOP_LESSONS: usize = 3;
/// 2d: character cap for one lesson excerpt in the fleet evidence block.
const FLEET_LESSON_CHARS: usize = 120;

/// 2d: fleet-wide cross-agent evidence — a SINGLE compact JSON block
/// (2b(b)): per-agent run metrics (runs, tool calls/errors, gave_up,
/// feedback, duration, tokens), the agent's newest tagged lessons, and
/// fleet skill usage (read in the window vs. exists-but-never-read).
///
/// Returns an empty string when there is no signal at all — no agent with
/// runs in the window, no agent-tagged lessons, no skill activity — in which
/// case there is nothing for a fleet review to judge.
pub fn fleet_evidence_json(
    manager: &MemoryManager,
    roster: &[(String, String)],
    window_days: u64,
) -> String {
    let window_days = window_days.max(1);
    let since = chrono::Utc::now() - chrono::Duration::days(window_days as i64);
    let log = crate::agents::metrics::MetricsLog::default();

    let mut agents: Vec<serde_json::Value> = Vec::new();
    for file_name in log.agent_names() {
        // Metrics file names are normalized (agent_file_name); map back to
        // the real profile name from the roster so the LLM can name the
        // agent in its suggestions (case-insensitive, first match).
        let display_name = roster
            .iter()
            .find(|(n, _)| crate::agents::metrics::agent_file_name(n) == file_name)
            .map(|(n, _)| n.clone())
            .unwrap_or(file_name);
        let plain_name = display_name;
        let name = plain_name.clone();
        let s = log.summary_since(&name, Some(since));
        // The chat profile has metrics (it is the UI's default agent identity)
        // but no backing profile file — tell the fleet improver that a
        // suggestion targeting it must be a prompt change, never a new_agents
        // creation (approving one would fail with "profile not found" and the
        // duplicate would shadow the real chat identity in the selector).
        let chat_note = if name.eq_ignore_ascii_case(CHAT_PROFILE_NAME) {
            format!(
                " (note: '{}' is the chat profile — it has no backing .json file; \
                 suggest prompt_change for it, never new_agents)",
                CHAT_PROFILE_NAME
            )
        } else {
            String::new()
        };
        // The agent's newest tagged lessons (capped) — the only per-agent
        // free-text content in the block besides the deterministic metrics.
        // (Tag lookup uses the PLAIN profile name: the chat_note suffix only
        // decorates the name shown in the JSON below.)
        let mut tagged: Vec<MemoryEntry> = manager
            .get_by_tag(&format!("agent:{plain_name}"))
            .into_iter()
            .filter(|m| matches!(m.r#type, crate::memory::MemoryType::Lesson))
            .collect();
        tagged.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        let lessons: Vec<String> = tagged
            .into_iter()
            .take(FLEET_TOP_LESSONS)
            .map(|m| truncate_to(&m.content, FLEET_LESSON_CHARS))
            .collect();
        if s.runs == 0 && lessons.is_empty() {
            continue;
        }
        let error_pct = if s.tool_calls == 0 {
            0.0
        } else {
            (s.tool_errors as f64 / s.tool_calls as f64 * 1_000.0).round() / 10.0
        };
        agents.push(serde_json::json!({
            "name": format!("{name}{chat_note}"),
            "runs": s.runs,
            "tool_calls": s.tool_calls,
            "tool_errors": s.tool_errors,
            "error_pct": error_pct,
            "gave_up": s.gave_up,
            "feedback_up": s.feedback_up,
            "feedback_down": s.feedback_down,
            "avg_duration_s": s.avg_duration_secs(),
            "tokens_in": s.tokens_in,
            "tokens_out": s.tokens_out,
            "top_lessons": lessons,
        }));
    }


    // Fleet skill usage (3a/3b data): what was read in the window, and what
    // exists in the store but was never read — the retire signal, as data.
    let skills_read: Vec<String> = log.skill_usage_since(Some(since)).into_iter().take(10).collect();
    let skills_never_read: Vec<String> = crate::memory::skills::SkillStore::default()
        .list()
        .into_iter()
        .map(|m| m.name)
        .filter(|n| !skills_read.iter().any(|u| u == n))
        .take(10)
        .collect();

    if agents.is_empty() && skills_read.is_empty() && skills_never_read.is_empty() {
        return String::new();
    }

    // Budget: lessons are the bulk of the block — drop them first, keep the
    // metrics (they are what a fleet review is FOR).
    let build = |with_lessons: bool| {
        let agents_json: Vec<serde_json::Value> = agents
            .iter()
            .map(|a| {
                if with_lessons {
                    a.clone()
                } else {
                    let mut a = a.clone();
                    a["top_lessons"] = serde_json::json!([]);
                    a
                }
            })
            .collect();
        serde_json::json!({
            "window_days": window_days,
            "agents": agents_json,
            "skills_read": skills_read,
            "skills_never_read": skills_never_read,
        })
        .to_string()
    };

    let mut json = build(true);
    if json.len() > FLEET_EVIDENCE_CHAR_BUDGET {
        json = build(false);
    }
    if json.len() > FLEET_EVIDENCE_CHAR_BUDGET {
        let mut t: String = json.chars().take(FLEET_EVIDENCE_CHAR_BUDGET).collect();
        t.push_str("… [truncated]");
        json = t;
    }
    json
}

/// 2d: hard character cap (the fleet evidence block's lesson excerpts).
pub(crate) fn truncate_to(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max_chars).collect();
        t.push('…');
        t
    }
}

/// 2d: the fleet-wide review (2b(b)) — the cross-agent counterpart of
/// `suggest_improvements`.
///
/// `roster` is the (name, description) of every known agent profile; the
/// caller holds the `AgentManager` (the `MemoryManager` does not know about
/// profiles). Evidence is the compact [`fleet_evidence_json`] block — a
/// single JSON summary (~≤2k tokens, 2d). The LLM looks for CROSS-agent
/// patterns a single-agent review would miss: the same failure across
/// several agents (→ a shared skill, or the per-agent change each needs), a
/// capability gap a new SHARED agent could fill (`new_agents`), and
/// fleet-wide skill maintenance.
///
/// The output is the SAME suggestion JSON as the per-agent path — each
/// suggestion names the agent it targets (`agent_name`) — so the review
/// panel and the `AppEvent::ImprovementSuggested` plumbing handle it
/// unchanged.
pub async fn suggest_fleet_improvements(
    manager: &MemoryManager,
    roster: &[(String, String)],
    focus: Option<&str>,
    llm_client: &dyn LlmClient,
) -> Result<Vec<ImprovementSuggestion>, String> {
    if !manager.config().auto_improve {
        tracing::debug!("auto_improve is off; skipping fleet improvement check");
        return Ok(Vec::new());
    }
    let window_days = manager.config().improvement_metrics_window_days.max(1) as u64;
    let evidence = fleet_evidence_json(manager, roster, window_days);
    if evidence.is_empty() {
        tracing::debug!("no fleet evidence in the window; skipping fleet improvement check");
        return Ok(Vec::new());
    }

    let roster_text = if roster.is_empty() {
        "(no agent profiles registered)".to_string()
    } else {
        let existing_list = roster
            .iter()
            .map(|(n, d)| {
                let d: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("{n} ({})", truncate_to(&d, 160))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let roster_block = roster
            .iter()
            .map(|(n, d)| {
                let d: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("{n}: {}", truncate_to(&d, 160))
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The synthetic chat profile exists (it is the UI's default identity,
        // configured through the chat settings) but has no backing .json file
        // in any agents dir — listing it among the real profiles would let the
        // LLM propose a NEW agent named "chat" (approving one would fail with
        // "profile not found" and the duplicate would shadow the real chat
        // identity in the selector).
        format!(
            "Existing agent profiles (do NOT propose these as new agents): {existing_list}. \
             Note: the chat profile ('{CHAT_PROFILE_NAME}') also exists — it is the UI's default \
             agent, configured through the chat settings; it has no backing .json file, so a \
             suggestion for it must be a prompt_change (never a new_agents entry). \
             \n\nRoster (name: description):\n{roster_block}"
        )
    };

    let focus_line = focus
        .map(|f| format!("\nConcentrate the review on: {f}"))
        .unwrap_or_default();

    let prompt = format!(
        "You are reviewing a FLEET of AI agent profiles in WuffAgent. Look for CROSS-AGENT \
         patterns that a single-agent review would miss.\n\n\
         Known agent profiles:\n{roster_text}\n\
         {focus_line}\n\n\
         Fleet evidence (last {window_days} day(s), single JSON block — per-agent run metrics, \
         tool error rates, durations, tokens, top lessons, plus skill usage):\n{evidence}\n\n\
         What to look for:\n\
         1. The SAME failure pattern across several agents (repeated tool errors, same misbehavior) → \
         propose a shared skill (skill_updates) capturing the fix, and/or one suggestion per \
         affected agent (its agent_name set) with the prompt/tool change it needs.\n\
         2. A capability gap that recurs across agents → propose a NEW SHARED agent (new_agents) \
         the fleet can hand off to.\n         2b. The chat profile ('{CHAT_PROFILE_NAME}') has no backing profile file - target it with \n         prompt_change, never new_agents (its run metrics may make it look like an existing profile; \n         approving a new 'chat' agent would fail and shadow the real chat identity).\n\
         3. Skills that exist but were never read fleet-wide (skills_never_read) → propose \
         retiring (action \"delete\") the ones with no clear ongoing value; merge heavily \
         overlapping skills.\n\
         4. An agent clearly outperforming its siblings → suggest what the others could borrow \
         (prompt style, tool allowlist).\n\n\
         Rules:\n\
         - Prefer few, high-confidence suggestions. If nothing rises above the noise, return an \
         empty array [].\n\n\
         Return a JSON array of suggestions (same shape as a single-agent review; empty [] if \
         nothing to improve):\n\
         [\n\
           {{\n\
             \"agent_name\": \"<profile to change>\",\n\
             \"prompt_change\": \"the FULL new system prompt text (replace the current one shown above) or null if no change needed\",\n\
             \"rationale\": \"why this change is needed (cite the fleet evidence)\",\n\
             \"description\": null,\n\
             \"allowed_tools\": null,\n\
             \"reasoning_effort\": null,\n\
             \"shell_config\": null,\n\
             \"handoff_targets\": null,\n\
             \"task_timeout_ms\": null,\n\
             \"new_agents\": [\n\
               {{\"name\": \"agent_name\", \"description\": \"...\", \"system_prompt\": \"...\", \"allowed_tools\": [\"tool1\", \"tool2\"]}}\n\
             ],\n\
             \"skill_updates\": [\n\
               {{\"action\": \"new|update|delete\", \"name\": \"skill-slug\", \"description\": \"one line\", \"when_to_use\": \"when this skill applies\", \"body\": \"markdown steps\"}}\n\
             ]\n\
           }}\n\
         ]\n\
         \n\
         Return [] if no improvements are needed.",
    );

    let messages = vec![Message {
        role: "user".to_string(),
        content: prompt,
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    }];

    let check_start = std::time::Instant::now();
    let (response, check_usage) = llm_client.complete_with_usage(&messages).await?;
    let check_duration_ms = check_start.elapsed().as_millis() as u64;
    let (check_tokens_in, check_tokens_out) = check_usage
        .as_ref()
        .map(|u| (u.prompt_tokens as u64, u.completion_tokens as u64))
        .unwrap_or((0, 0));

    // Parse (same tolerant JSON extraction as the per-agent path).
    let trimmed = response.trim();
    let mut suggestions = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<ImprovementSuggestion>>(trimmed)
            .map_err(|e| format!("Failed to parse fleet improvement suggestions: {}", e))?
    } else {
        let start = trimmed.find('[').unwrap_or(0);
        let end = trimmed.rfind(']').unwrap_or(trimmed.len());
        let json = &trimmed[start..=end];
        serde_json::from_str::<Vec<ImprovementSuggestion>>(json)
            .map_err(|e| format!("Failed to parse fleet improvement suggestions: {}", e))?
    };

    // 1c: record the loop's own cost for this fleet-wide check (best-effort —
    // never fails the check).
    crate::agents::metrics::record_check(
        crate::agents::metrics::FLEET_FILE_STEM,
        "fleet",
        check_tokens_in,
        check_tokens_out,
        suggestions.len(),
        check_duration_ms,
    );

    // I3-style: attach the deterministic evidence (the fleet block itself,
    // display-truncated) so the panel shows WHY each suggestion was made.
    let evidence_lines = vec![truncate_for_evidence(&evidence)];
    for s in &mut suggestions {
        s.evidence = evidence_lines.clone();
    }

    // Synthetic-profile guard (fleet variant): the LLM occasionally names a
    // profile that exists only in the metrics (e.g. "chat" — the UI's
    // default identity, which has no backing file) or drifts on the
    // spelling/case of a roster entry. Re-target those onto the nearest
    // roster entry so approve can find a real profile to write.
    sanitize_fleet_agent_names(&mut suggestions, roster);

    if suggestions.is_empty() {
        tracing::debug!("No improvements suggested by the fleet review");
    } else {
        tracing::info!(
            "Generated {} improvement suggestion(s) from the fleet review",
            suggestions.len()
        );
    }
    Ok(suggestions)
}

/// 2d: re-target suggestions whose `agent_name` does not match a profile in
/// the `roster` (case-insensitive) onto the best-matching roster entry by
/// name similarity (a Dice coefficient over the lowercased names, matching
/// the metrics file-name normalisation).
///
/// Rationale: the fleet review sees agent names from the metrics (which
/// include synthetic identities like "chat" — the UI's default chat profile
/// has no backing file in any agents dir). A suggestion targeting such a
/// name fails on approve with "profile not found in any agents directory —
/// nothing was written". The fleet prompt asks the LLM to name the profile
/// it changes, and the roster IS the set of real profiles, so a
/// near-miss name is almost certainly a spelling/case drift of a roster
/// entry (e.g. "Orchestrator" vs "orchestrator") — snap it to the real
/// one. Exact matches are left untouched.
pub(crate) fn sanitize_fleet_agent_names(suggestions: &mut [ImprovementSuggestion], roster: &[(String, String)]) {
    if suggestions.is_empty() || roster.is_empty() {
        return;
    }
    for s in suggestions.iter_mut() {
        let exact = roster
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(&s.agent_name))
            // The chat profile is an EXISTING (synthetic) profile — the
            // prompt declares it and the panel applies a prompt_change for
            // it to the chat settings — so a "chat" suggestion is kept, not
            // re-targeted onto a fuzzy-matched roster entry.
            || s.agent_name.eq_ignore_ascii_case(CHAT_PROFILE_NAME);
        if exact {
            continue;
        }
        // Best fuzzy match: highest Dice coefficient over the lowercased
        // names, ignoring separators (spaces, underscores, hyphens) so
        // "sub session" and "subsession" match. Requires a minimum
        // similarity (0.6) to avoid snapping an unrelated name onto a
        // random roster entry; otherwise the suggestion keeps its original
        // name (and fails on approve, as before).
        let norm = |s: &str| -> String {
            s.to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect()
        };
        let target = norm(&s.agent_name);
        let mut best: Option<(f64, String)> = None;
        for (n, _) in roster.iter() {
            let a = norm(n);
            if a.is_empty() || target.is_empty() {
                continue;
            }
            let score = dice(&a, &target);
            let is_better = best.as_ref().map_or(true, |(b, _)| score > *b);
            if is_better {
                best = Some((score, n.clone()));
            }
        }
        if let Some((score, name)) = best {
            if score >= 0.6 {
                tracing::info!(
                    "Fleet improvement targets unknown profile '{}' — re-targeting to roster entry '{}' (similarity {:.2})",
                    s.agent_name,
                    name,
                    score
                );
                s.agent_name = name;
            }
        }
    }
}

/// Dice coefficient over two strings (character-bag overlap), in [0, 1].
/// `dice("abc", "abc") == 1.0`; `dice("abc", "") == 0.0`.
pub(crate) fn dice(a: &str, b: &str) -> f64 {
    use std::collections::HashMap;
    let count = |s: &str| {
        let mut m: HashMap<char, usize> = HashMap::new();
        for c in s.chars() {
            *m.entry(c).or_insert(0) += 1;
        }
        m
    };
    let ca = count(a);
    let cb = count(b);
    let overlap: usize = ca
        .iter()
        .map(|(c, n)| (*n).min(cb.get(c).copied().unwrap_or(0)))
        .sum();
    let la: usize = ca.values().sum();
    let lb: usize = cb.values().sum();
    if la == 0 || lb == 0 {
        return 0.0;
    }
    2.0 * overlap as f64 / (la + lb) as f64
}
