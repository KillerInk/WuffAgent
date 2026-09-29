//! The auto-improvement loop's half of `MemoryManager` (extracted from
//! manager.rs, modularization phase 6): the on-demand/per-task check
//! wrappers, the persisted per-agent state, and the evidence gate. The core
//! memory store (search/get/add/…) stays in the parent `manager` module;
//! this is its CHILD module, so it can use the parent's private fields
//! (`storage_path`) without any visibility changes.

use super::MemoryManager;
use crate::agents::improvement::{no_op_backoff_multiplier, suggest_improvements};
use crate::memory::types::{ImprovementStatus, MemoryType};
use std::path::PathBuf;

/// Cached current-thread tokio runtime for blocking on the on-demand
/// improvement check from sync callers (a UI-spawned thread, or a tool that
/// already has no ambient runtime). The LLM call's own HTTP timeout usually
/// fires first; this is only a backstop.
static IMPROVEMENT_CHECK_RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build improvement-check runtime")
    });

/// Run an async future to completion. Reuses the ambient runtime handle when
/// one is available (e.g. inside `spawn_blocking`), otherwise the cached
/// [`IMPROVEMENT_CHECK_RUNTIME`] (e.g. on a plain UI-spawned thread, or in
/// tests). Same pattern the `run_self_improvement` tool used before 4b.
fn block_on_improvement<F>(fut: F) -> F::Output
where
    F: std::future::Future,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => IMPROVEMENT_CHECK_RUNTIME.block_on(fut),
    }
}

/// Hard ceiling for one on-demand improvement check (backstop against a
/// wedged LLM client holding the caller's thread).
const IMPROVEMENT_CHECK_TIMEOUT_SECS: u64 = 180;

impl MemoryManager {
    /// Suggest improvements for an agent based on memories and recent task.
    pub async fn suggest_improvements(
        &self,
        agent_config: &crate::agents::config::AgentConfig,
        task: &str,
        result: &str,
        stats: &crate::agents::RunStats,
    ) -> Result<Vec<crate::types::ImprovementSuggestion>, String> {
        let llm = match &self.llm_client {
            Some(c) => c.clone(),
            None => return Ok(Vec::new()),
        };
        suggest_improvements(self, agent_config, task, result, stats, llm.as_ref()).await
    }

    /// 4b: run ONE on-demand self-improvement check for `agent_config`,
    /// bypassing the per-task cooldown/evidence gates (the `AgentEngine`
    /// per-task path uses the gated `suggest_improvements` directly). This is
    /// the BLOCKING wrapper for callers outside an async context — the
    /// `run_self_improvement` tool and the UI's "run check now" button both
    /// call it, so the timeout / runtime / record bookkeeping lives in one
    /// place.
    ///
    /// Blocks until the LLM call completes or times out
    /// (`IMPROVEMENT_CHECK_TIMEOUT_SECS`). Returns `Ok(suggestions)` when the
    /// check ran to term (possibly an empty list = nothing to improve), and
    /// `Err(message)` on timeout or LLM failure (logged; the message is
    /// concise so a caller can prefix it with its own context).
    ///
    /// The per-agent check is recorded (2a) ONLY when the check ran to term,
    /// so a timed-out/failed check does not re-arm the evidence gate or bump
    /// the no-op streak. Callers are expected to check `config().auto_improve`
    /// first (the tool reports that case explicitly; the UI disables its
    /// button).
    pub fn run_improvement_check(
        &self,
        agent_config: &crate::agents::config::AgentConfig,
        focus: Option<&str>,
    ) -> Result<Vec<crate::types::ImprovementSuggestion>, String> {
        let fallback_task = format!(
            "(on-demand self-improvement check for '{}' — no single task in context)",
            agent_config.name
        );
        let task = focus.unwrap_or(&fallback_task);
        let result =
            "(no task result; judge the agent from its lesson memories, run metrics and \
             the effect check below)"
                .to_string();
        let stats = crate::agents::RunStats::default();
        let fut = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(IMPROVEMENT_CHECK_TIMEOUT_SECS),
                self.suggest_improvements(agent_config, &task, &result, &stats),
            )
            .await
        };
        let check = match block_on_improvement(fut) {
            Ok(Ok(suggestions)) => suggestions,
            Ok(Err(e)) => {
                tracing::warn!(agent = %agent_config.name, error = %e, "on-demand improvement check failed");
                return Err(format!("failed: {e}"));
            }
            Err(_) => {
                tracing::warn!(agent = %agent_config.name, "on-demand improvement check timed out");
                return Err(format!(
                    "timed out after {IMPROVEMENT_CHECK_TIMEOUT_SECS}s"
                ));
            }
        };
        // Only a check that ran to term re-arms the evidence gate / tracks the
        // no-op streak.
        self.record_agent_improvement_check(&agent_config.name, !check.is_empty());
        Ok(check)
    }

    /// 2d: fleet-wide variant of [`Self::suggest_improvements`] (2b(b)) —
    /// the LLM comes from the manager's own client (no client = no
    /// suggestions, same as the per-agent path). `roster` is the
    /// (name, description) of every known agent profile; the caller holds
    /// the AgentManager.
    pub async fn suggest_fleet_improvements(
        &self,
        roster: &[(String, String)],
        focus: Option<&str>,
    ) -> Result<Vec<crate::types::ImprovementSuggestion>, String> {
        let llm = match &self.llm_client {
            Some(c) => c.clone(),
            None => return Ok(Vec::new()),
        };
        crate::agents::improvement::suggest_fleet_improvements(self, roster, focus, llm.as_ref()).await
    }

    /// 2d: fleet-wide variant of [`Self::run_improvement_check`] — the
    /// BLOCKING wrapper for the `run_self_improvement` tool (scope "fleet")
    /// and the UI's "fleet" run-check option. Same timeout / runtime /
    /// record semantics; the check is recorded under the pseudo-agent name
    /// "fleet" (visible in `list_improvement_status` and the panel header).
    pub fn run_fleet_improvement_check(
        &self,
        roster: &[(String, String)],
        focus: Option<&str>,
    ) -> Result<Vec<crate::types::ImprovementSuggestion>, String> {
        let fut = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(IMPROVEMENT_CHECK_TIMEOUT_SECS),
                self.suggest_fleet_improvements(roster, focus),
            )
            .await
        };
        let check = match block_on_improvement(fut) {
            Ok(Ok(suggestions)) => suggestions,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "on-demand FLEET improvement check failed");
                return Err(format!("failed: {e}"));
            }
            Err(_) => {
                tracing::warn!("on-demand FLEET improvement check timed out");
                return Err(format!("timed out after {IMPROVEMENT_CHECK_TIMEOUT_SECS}s"));
            }
        };
        // Only a check that ran to term tracks the no-op streak (2d).
        self.record_agent_improvement_check("fleet", !check.is_empty());
        Ok(check)
    }

    /// I4 (cost control): whether new improvement evidence has arrived since
    /// the last improver check.
    ///
    /// Evidence is any **Lesson-type** entry — that covers every kind the
    /// improver learns from (S1 verification outcomes, S2 user feedback,
    /// S3 agent lessons, F5 rejection lessons). Counted across all agents.
    ///
    /// When no check has been recorded yet (missing or corrupt state file),
    /// evidence exists if ANY lesson exists at all — the first check then
    /// records the baseline.
    pub fn has_new_improvement_evidence(&self) -> bool {
        let last_check = self
            .load_state_doc()
            .last_check
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));
        let memories = self.get_all_memories();
        match last_check {
            // Some Lesson entry strictly newer than the last check.
            Some(ts) => memories.iter().any(|e| {
                e.r#type == MemoryType::Lesson && e.timestamp.map(|t| t > ts).unwrap_or(false)
            }),
            None => memories.iter().any(|e| e.r#type == MemoryType::Lesson),
        }
    }

    /// I4 (cost control): record that an (agent-agnostic) improvement check
    /// just ran, so the global evidence gate stays closed until new Lesson
    /// entries arrive. Kept for the global status view and legacy v1
    /// semantics; the per-task path uses `record_agent_improvement_check`.
    ///
    /// Best-effort: any failure is only logged — the state file must never
    /// be able to break task completion.
    pub fn record_improvement_check(&self) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        doc.last_check = Some(chrono::Utc::now().timestamp());
        self.save_state_doc(&doc);
    }

    // ── 2a: per-agent improvement state ────────────────────────────────────

    /// 2a: this agent's improvement-loop state. The legacy v1 global
    /// `last_check` acts as a fallback BASELINE (evidence gate) for agents
    /// that never had a check of their own; `runs_since_check` and friends
    /// are per-agent only.
    pub fn agent_improvement_state(&self, agent: &str) -> crate::memory::types::AgentImprovementState {
        let doc = self.load_state_doc();
        let rec = doc.agents.get(agent).cloned().unwrap_or_default();
        crate::memory::types::AgentImprovementState {
            last_check: rec
                .last_check
                .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
                .or_else(|| {
                    doc.last_check
                        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                }),
            runs_since_check: rec.runs_since_check,
            no_op_streak: rec.no_op_streak,
            last_effect_verdict: rec.last_effect_verdict,
        }
    }

    /// 2a: count one completed task of `agent` toward the per-agent
    /// cooldown (called by the engine after every task, only while
    /// `auto_improve` is on).
    pub fn record_agent_task_completed(&self, agent: &str) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        doc.agents.entry(agent.to_string()).or_default().runs_since_check += 1;
        self.save_state_doc(&doc);
    }

    /// 2a: whether `agent`'s per-task cooldown has elapsed.
    ///
    /// Backoff: a no-op streak (consecutive checks that produced no
    /// suggestions) multiplies the base cooldown — streak 1: x1, 2: x2,
    /// 3: x3, 4+: x4 — so an agent whose lessons keep arriving but never
    /// yield a suggestion is re-checked less often; any productive check
    /// (or an applied one) resets the streak to 0.
    ///
    /// 2f: wall-clock floor (config `improvement_min_interval_hours` > 0):
    /// the check is ALSO not due until that much real time has passed since
    /// the agent's last check (per-agent timestamp, with the legacy v1
    /// global baseline as fallback — the same rule as the evidence gate).
    /// A burst of tasks can no longer burn through several LLM checks in a
    /// minute. 0 = off (the legacy pure task-count gate). The on-demand
    /// paths (`run_self_improvement`, the panel's "Run check") bypass this
    /// gate entirely.
    pub fn agent_improvement_due(&self, agent: &str, base_cooldown_tasks: usize) -> bool {
        let base = base_cooldown_tasks.max(1) as u64;
        let doc = self.load_state_doc();
        let rec = doc.agents.get(agent);
        let runs = rec.map_or(0, |r| r.runs_since_check);
        let streak = rec.map_or(0, |r| r.no_op_streak);
        let mult = no_op_backoff_multiplier(streak);
        if runs < base * mult {
            return false;
        }
        let min_hours = self.config().improvement_min_interval_hours;
        if min_hours > 0 {
            let last = rec
                .and_then(|r| r.last_check)
                .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
                .or_else(|| {
                    doc.last_check
                        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                });
            if let Some(last) = last {
                let elapsed_h = chrono::Utc::now().signed_duration_since(last).num_hours().max(0);
                if elapsed_h < min_hours as i64 {
                    return false;
                }
            }
        }
        true
    }

    /// 2a: record that an improvement check for `agent` just ran.
    /// `produced` = whether it yielded at least one suggestion (resets the
    /// no-op streak; an empty result extends it). Either way the per-agent
    /// cooldown counter restarts and the evidence gate baselines now.
    pub fn record_agent_improvement_check(&self, agent: &str, produced: bool) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        let rec = doc.agents.entry(agent.to_string()).or_default();
        // Milliseconds: lesson timestamps are sub-second (see AgentStateRec).
        rec.last_check = Some(chrono::Utc::now().timestamp_millis());
        rec.runs_since_check = 0;
        rec.no_op_streak = if produced { 0 } else { rec.no_op_streak.saturating_add(1) };
        self.save_state_doc(&doc);
    }

    /// 2a: persist the effect check's deterministic verdict for `agent`
    /// (written by `effect_check_section` after each check that has an
    /// applied-change marker to compare against).
    pub fn record_effect_verdict(&self, agent: &str, verdict: &str) {
        let mut doc = self.load_state_doc();
        doc.version = 2;
        doc.agents
            .entry(agent.to_string())
            .or_default()
            .last_effect_verdict = Some(verdict.to_string());
        self.save_state_doc(&doc);
    }

    /// 2a/1a: per-agent evidence gate — whether NEW evidence exists since
    /// THIS agent's last check: Lesson evidence (the fast path, OR-combined)
    /// OR a significant metric delta (1a). This is the gate the per-task auto
    /// check uses (`engine.rs`), so metric-only degradation now re-arms the
    /// loop even when the agent saves no lesson.
    pub fn has_new_agent_improvement_evidence(&self, agent: &str) -> bool {
        self.agent_lesson_evidence(agent) || self.agent_metric_evidence(agent).is_some()
    }

    /// 2a: the Lesson half of the per-agent evidence gate — whether a
    /// relevant Lesson entry (tagged `agent:<name>`, or agent-less/global) is
    /// newer than this agent's baseline. OTHER agents' lessons do not re-arm
    /// this agent.
    fn agent_lesson_evidence(&self, agent: &str) -> bool {
        let baseline = self.agent_improvement_state(agent).last_check;
        let agent_tag = format!("agent:{agent}");
        self.get_all_memories().iter().any(|e| {
            if e.r#type != MemoryType::Lesson {
                return false;
            }
            let relevant = e
                .tags
                .iter()
                .any(|t| t == &agent_tag)
                || !e.tags.iter().any(|t| t.starts_with("agent:"));
            if !relevant {
                return false;
            }
            match baseline {
                Some(ts) => e.timestamp.map(|t| t > ts).unwrap_or(false),
                None => true,
            }
        })
    }

    /// 1a: the metric half of the per-agent evidence gate — whether the
    /// run-metrics SINCE this agent's last check show a significant regression
    /// vs the matching preceding window (the same before-window rule as the
    /// effect check). Returns `Some(reason)` with a human-readable delta
    /// description (surfaced in `list_improvement_status`) when a re-arm is
    /// warranted, `None` otherwise. Never-checked agents (no baseline, and no
    /// v1 fallback) have no before/after to compare, so no metric delta — they
    /// rely on the Lesson gate (and the Phase-4 rare-agent safety net).
    pub fn agent_metric_evidence(&self, agent: &str) -> Option<String> {
        let baseline = self.agent_improvement_state(agent).last_check?;
        let metrics_log = crate::agents::metrics::MetricsLog::default();
        // 2d: the shared gate primitive — after `[since, now)` vs the
        // immediately preceding same-length window (min 1 day so a fresh
        // baseline still gets a window, max 30 days so an old baseline
        // doesn't sweep in months of history, mirroring
        // `effect_check_section`'s before-window rule). The status rendering
        // (`list_improvement_status`) calls the same fn, so gate and display
        // cannot diverge.
        let (after, before) = metrics_log.compare_since(agent, baseline);
        metric_evidence_reason(
            &before.summary,
            &after.summary,
            self.config().improvement_metric_evidence_runs,
        )
    }

    /// Load the improvement-check state document, tolerating a missing or
    /// corrupt file (both mean "default state"; a corrupt file is left in
    /// place for inspection — the next record overwrites it).
    fn load_state_doc(&self) -> ImprovementStateDoc {
        let path = self.improvement_state_path();
        if !path.exists() {
            return ImprovementStateDoc::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// 2a: atomic best-effort write of the state document (temp + rename,
    /// mirroring `save_memories`); failures are only logged — the state
    /// file must never be able to break task completion.
    fn save_state_doc(&self, doc: &ImprovementStateDoc) {
        let path = self.improvement_state_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::debug!("[MEMORY] Could not create state dir {:?}: {}", parent, e);
                return;
            }
        }
        let content = match serde_json::to_string(doc) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("[MEMORY] Could not serialize improvement state: {}", e);
                return;
            }
        };
        let temp_path = path.with_extension("json.tmp");
        if let Err(e) =
            std::fs::write(&temp_path, content).and_then(|_| std::fs::rename(&temp_path, &path))
        {
            let _ = std::fs::remove_file(&temp_path);
            tracing::debug!(
                "[MEMORY] Could not write improvement state {:?}: {}",
                path,
                e
            );
        }
    }

    /// I4: the improvement-check state file, a sibling of the project memory
    /// file (e.g. `improvement_state.json` next to `default.json`).
    fn improvement_state_path(&self) -> PathBuf {
        self.storage_path
            .parent()
            .map(|p| p.join("improvement_state.json"))
            .unwrap_or_else(|| PathBuf::from("improvement_state.json"))
    }

    /// 1c/2a: the improvement-loop state snapshot for the
    /// `list_improvement_status` tool. Read-only and side-effect free — the
    /// persisted state (global + per-agent) plus the live evidence gate and
    /// config. Per-agent `last_check` here is the agent's OWN (no legacy
    /// fallback — the fallback is a baseline detail of the evidence gate,
    /// not something the status view should present).
    pub fn improvement_status(&self) -> ImprovementStatus {
        let config = self.config();
        let memories = self.get_all_memories();
        let doc = self.load_state_doc();
        ImprovementStatus {
            last_check: doc
                .last_check
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0)),
            has_new_evidence: self.has_new_improvement_evidence(),
            auto_improve: config.auto_improve,
            improvement_cooldown_tasks: config.improvement_cooldown_tasks,
            improvement_min_interval_hours: config.improvement_min_interval_hours,
            lesson_count: memories
                .iter()
                .filter(|e| e.r#type == MemoryType::Lesson)
                .count(),
            agents: doc
                .agents
                .iter()
                .map(|(name, rec)| {
                    (
                        name.clone(),
                        crate::memory::types::AgentImprovementState {
                            last_check: rec
                                .last_check
                                .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms)),
                            runs_since_check: rec.runs_since_check,
                            no_op_streak: rec.no_op_streak,
                            last_effect_verdict: rec.last_effect_verdict.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// 2a: persisted state of the auto-improvement loop (unix seconds).
///
/// v1 (I4) stored a single GLOBAL `last_check`. v2 keeps that field as a
/// LEGACY fallback baseline (an agent with no per-agent entry of its own
/// uses it as its evidence baseline) and adds per-agent counters, so a busy
/// agent can no longer starve an idle one's checks (or vice versa). Both
/// file shapes parse into this struct (`#[serde(default)]` everywhere), so
/// upgrading a v1 file is just reading it — the next write re-serializes it
/// as v2.
///
/// Private: only the `record_*` / `agent_improvement_*` manager methods
/// touch it.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
struct ImprovementStateDoc {
    #[serde(default)]
    version: u32,
    /// v1 global last-check (unix seconds) — legacy fallback baseline.
    #[serde(default)]
    last_check: Option<i64>,
    /// 2a: per-agent state, keyed by agent profile name.
    #[serde(default)]
    agents: std::collections::BTreeMap<String, AgentStateRec>,
}

/// On-disk per-agent record. `last_check` is unix MILLISECONDS (the lesson
/// timestamps carry sub-second precision — a seconds-precision baseline
/// would mark same-second lessons as "newer" than the check). Converted to
/// the public `AgentImprovementState` at the API boundary.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
struct AgentStateRec {
    #[serde(default)]
    last_check: Option<i64>,
    #[serde(default)]
    runs_since_check: u64,
    #[serde(default)]
    no_op_streak: u32,
    #[serde(default)]
    last_effect_verdict: Option<String>,
}

/// 1a: metric-delta evidence thresholds (v1 constants — promoted to config
/// knobs only if tuning asks; the run floor IS a knob:
/// `MemoryConfig::improvement_metric_evidence_runs`).
const METRIC_EVIDENCE_ERR_RATE_DELTA_PP: f64 = 5.0; // tool-error-rate jump (pp)
const METRIC_EVIDENCE_GAVE_UP_DELTA_PP: f64 = 20.0; // gave-up share jump (pp)
const METRIC_EVIDENCE_DURATION_DELTA_FRAC: f64 = 0.5; // avg-duration jump (×)

/// 1a: pure helper for the metric evidence gate. Given the before/after
/// windows (matching lengths, as `agent_metric_evidence` builds them) and the
/// run floor, return `Some(reason)` when the after-window is judgeable
/// (≥ floor runs) AND at least one metric regressed beyond its threshold:
/// tool-error-rate delta ≥ 5pp, OR gave-up share delta ≥ 20pp, OR avg-duration
/// delta ≥ +50%. `None` when the floor isn't met (too few runs) or nothing
/// regressed — so 1–2 noisy runs can't re-arm the LLM.
fn metric_evidence_reason(
    before: &crate::agents::metrics::MetricsSummary,
    after: &crate::agents::metrics::MetricsSummary,
    run_floor: u32,
) -> Option<String> {
    // The run floor: 1–2 noisy after-runs must not re-arm an LLM call.
    if after.runs < run_floor {
        return None;
    }
    // A delta needs a real baseline: an empty before-window is not a 0%
    // baseline — with nothing to compare against there is no "change".
    if before.runs == 0 {
        return None;
    }
    let mut reasons: Vec<String> = Vec::new();

    // Tool-error-rate delta (percentage points), only when BOTH windows have
    // enough tool calls for the rate to be stable (a 1-call window is noise).
    // Fires on an INCREASE (degradation); an improvement is not a re-arm.
    const MIN_CALLS: u32 = 10;
    if before.tool_calls >= MIN_CALLS && after.tool_calls >= MIN_CALLS {
        let err_before = 100.0 * before.tool_errors as f64 / before.tool_calls as f64;
        let err_after = 100.0 * after.tool_errors as f64 / after.tool_calls as f64;
        if err_after - err_before >= METRIC_EVIDENCE_ERR_RATE_DELTA_PP {
            reasons.push(format!("tool-error rate {err_before:.0}% → {err_after:.0}%"));
        }
    }

    // Gave-up share delta (percentage points of runs); fires on an increase.
    // (`before.runs >= 1` is guaranteed by the empty-baseline guard above.)
    let gu_before = 100.0 * before.gave_up as f64 / before.runs as f64;
    let gu_after = 100.0 * after.gave_up as f64 / after.runs as f64;
    if gu_after - gu_before >= METRIC_EVIDENCE_GAVE_UP_DELTA_PP {
        reasons.push(format!("gave-up share {gu_before:.0}% → {gu_after:.0}%"));
    }

    // Average run-duration delta (fraction); fires on a ≥50% slowdown.
    let avg_before = before.total_duration_ms as f64 / before.runs as f64;
    let avg_after = after.total_duration_ms as f64 / after.runs as f64;
    if avg_before > 0.0 && avg_after >= avg_before * (1.0 + METRIC_EVIDENCE_DURATION_DELTA_FRAC) {
        reasons.push(format!(
            "avg duration {:.1}s → {:.1}s",
            avg_before / 1000.0,
            avg_after / 1000.0
        ));
    }

    match reasons.is_empty() {
        true => None,
        false => Some(reasons.join(", ")),
    }
}
