# Context-Rot Prevention After Trimming

Created: 2026-09-27. Status: **S1+S2+S3 DONE** (2026-09-27), S4 pending (stretch).

Goal: stop the model from "going off the rails" after the context window is
reached and history is trimmed — it should keep the TASK, the user's
CORRECTIONS, the DECISIONS, and the CURRENT STATE, and know that compaction
happened.

## The rot gaps in the current pipeline (evidence)

`trim_messages` (wuffagent-core/src/trimming/summarizer/mod.rs:148) runs:
freshness pass (stale read_file pairs removed) → `age_sweep` (oldest-first
removal) → `summarize_old_tool_messages` (in-place tool-result summarization)
→ `truncate_largest_message` (halve → 39-char placeholder) → last-resort tail
halving. Trigger/target: 90% / 50% of n_ctx (client `TRIM_TRIGGER_PCT` /
`TRIM_TARGET_PCT`, applied in agents/agent/loop.rs:223-277).

1. **The task itself is droppable.** `age_sweep` (trim.rs:117-122) protects
   only the LAST user message and the leading system prompt. The FIRST user
   message — the actual task — is the OLDEST and is removed first. Goal drift
   is the #1 rot cause.
2. **Old user messages (follow-ups, "no, do X instead") are removed as plain
   turns** (trim.rs:149). Corrections are exactly what must not be lost.
3. **Assistant turns carrying the plan/rationale die with their tool pairs** —
   no trace of the "why" remains.
4. **No re-anchoring signal.** After a big drop the model just sees a shorter
   list: it believes it still knows what was trimmed → re-runs commands,
   contradicts earlier decisions, "I already checked X" when the check was
   trimmed.
5. **The 90%→50% cliff:** when the trigger fires, ~40% of the context vanishes
   in one jump. Bigger jump = bigger rot per event.
6. **Unmeasured:** trims only go to tracing. The metrics log has no trim
   events, so we cannot correlate "trim happened" with later verification
   failures / tool-error spikes (the self-improvement loop can't learn).

## Design: the Mission Brief (rolling state summary that survives trimming)

New module `wuffagent-core/src/trimming/brief.rs` — **pure functions, no LLM,
no new deps** (keeps the trim path synchronous and testable):

```rust
pub struct SessionBrief {
    pub task: String,            // first user msg, verbatim (cap 800)
    pub corrections: Vec<String>,// later user msgs (cap 400 each, max 6, newest kept)
    pub completed: Vec<String>,  // assistant bullet/done lines (cap 120 each, max 12)
    pub in_progress: Option<String>, // most recent "next/currently" line (cap 300)
    pub decisions: Vec<String>,  // "decided/chose/will use/because" lines (cap 200, max 8)
    pub files_touched: Vec<String>,  // write_file/apply_diff/mkdir/move/delete paths (cap 12)
}
```

- `extract_brief(dropped: &[Message], prev: Option<&SessionBrief>) -> SessionBrief`
  — merges the dropped span into the previous brief (cumulative). Overflow
  drops OLDEST lines; `task` + newest corrections are never dropped.
- `render_brief(&SessionBrief) -> Message` — ONE user-role message with a
  stable marker prefix `[SESSION BRIEF` (so `is_brief_message()` can find it):

  ```
  [SESSION BRIEF — the earlier part of this conversation was compacted to fit
  the context window. Current state:
  Task: ...
  Done: - ...
  In progress: ... (continue here)
  Decisions: - ...
  User notes/corrections: - ...
  Files touched: - path (edit) ...
  If you need a detail that is not listed here, re-read the file or re-run the
  command instead of assuming it.]
  ```

- Wiring in `trim_messages` (small, additive):
  - `age_sweep` takes an extra `dropped: &mut Vec<Message>` out-param (moved,
    not cloned) and collects what it removes.
  - If `dropped` content exceeds ~1000 chars: re-extract the brief, replace
    any existing brief message, insert at index 1 (after the system prompt).
  - The brief is ANCHORED: `age_sweep` skips it (same guard as the last user
    message), and `truncate_largest_message` excludes it (like the leading
    system prompt). A second trim re-renders the same single message (idempotent,
    never stacks).
  - 2-line belt-and-braces: `age_sweep` also skips the FIRST user message (the
    task) — verbatim task text stays even though the brief carries it too.
  - Freshness-pass drops (stale file reads) are NOT extracted: they are stale
    by definition (the model re-reads the file when it matters).

**Why the brief as a message at index 1, not appended to the system prompt:**
mid-list user-role messages are universally accepted by servers; the system
prompt should stay the stable per-agent text (built by `build_system_prompt`),
and the brief changes per trim. (Prefix caching is broken by trimming itself
regardless.)

### Steps (each independently shippable + testable)

- [x] **S1 — Mission Brief v1 (deterministic).** `brief.rs` + wiring above.
      Unit tests: extraction (task/corrections/decisions/completed/files from a
      synthetic dropped span), merge + caps + never-drop-task, render marker,
      anchoring (brief survives a second trim, is replaced not stacked),
      first-user-message protection, no brief when nothing was dropped,
      idempotency. This is ~300-400 lines incl. tests; the biggest bang for the
      Deviations from the sketch above (final behavior):
      - Anchor: the brief is inserted right after the VERBATIM TASK (first
        non-system user message) when one exists, else at the system-1/front
        position — the task is the beginning of the conversation; the brief
        summarizes what happened since. (`apply_brief` in brief.rs.)
      - Fit guard: the brief is protected from every shrink stage, so it is
        NOT inserted when it alone does not fit `target_chars` (the budget
        would be unreachable). Tiny test budgets skip the brief; real budgets
        (50% of n_ctx) never do.
      - Test updates: `test_plain_chat_no_tool_messages_noop` 2→1 (first user
        message now protected); `test_tool_heavy_history_is_trimmed_under_
        budget` now gets a brief after its task (no system message).
       buck.
       DONE 2026-09-27. Implementation notes: stateless (no instance field) —
       the previous brief is re-parsed from the rendered message already in the
       list (`from_rendered`; render/parse contract); the task seed is the first
       NON-brief user message still in context; freshness-pass drops are not
       extracted (stale by definition). Tests: `brief.rs` unit tests (extraction,
       merge/caps, roundtrip, anchoring) + `summarizer/tests/brief.rs`
       (anchored-after-big-drop, rolling-not-stacked second trim, trivial-drop
       no-brief).
      buck.
- [x] **S2 — Measure rot.** DONE 2026-09-27. `MetricsLine::Trim`
      (ts, chars_before, chars_after, messages_removed, brief_updated,
      overflow) + `MetricsLog::log_trim` + `record_trim` writer hook; `MetricsSummary`
      gained `trims` (rendered in `format_labeled`, so the improver's
      before/after windows include trim counts). Written from the agent loop at
      BOTH trim sites: the proactive trigger→target trim (overflow=false) and
      the 400-exceed-context backstop (overflow=true). `brief_updated` records
      whether a mission brief was present in the post-trim context (the
      re-anchoring the model can rely on). NOTE: the first S2 diff accidentally
      replaced the `SkillUse` variant instead of adding `Trim` beside it —
      fixed in the same commit (lesson: additive enum changes need the old
      variant in the SEARCH context). LATER (not done): improver/effect-check
      correlation — runs after a trim → verification outcome + tool-error rate
      vs. baseline (the I5 pattern), feeding the self-improvement loop.
- [x] **S3 — Soften the cliff (config, no default behavior change).**
      DONE 2026-09-27. `TrimConfig.trim_trigger_pct` / `trim_target_pct`
      (u8, serde defaults 90/50 — old config files deserialize unchanged).
      `ChatClient` gained a per-run stamp (`set_trim_pcts`, like
      `set_agent_name`; the client is shared across agents) and
      `trim_trigger_chars` / `trim_target_chars` derive from it (defaults
      keep today's behavior for plain chat). `Agent::builder` stamps the
      agent's `TrimConfig` pcts before each run, so per-agent tuning is now
      just config: e.g. `"trim_trigger_pct": 65` for a softer first cut.
      Tests: serde-missing-field defaults + stamped-pct budget test
      (`client/tests/request.rs`). Once S2 data exists, tune per agent
      (e.g. 90→65 first cut, 50 hard floor) with evidence instead of
      guessing — the overflow=true share of Trim lines is the miscalibration
      signal to watch.
- [ ] **S4 — Stretch: agent-side scratchpad + LLM polish.**
      a) `session_note` tool: the agent explicitly records "state / next step";
         the loop re-injects the note right after the system prompt on every
         call (capped); the brief merges it with top priority. Strongest
         guarantee — survives ANY trim.
      b) Optional LLM brief polish behind a flag: when a dropped span is large
         and a client is available, refine the brief (request carries ONLY the
         dropped span + old brief — small, cannot overflow); deterministic
         extraction is always the fallback.

## Constraints / notes

- Trim path must stay synchronous + cheap: no LLM in S1, no extra disk I/O.
- The visible transcript (UI/session file) is never modified by trimming
  (existing invariant, memory 16702247) — the brief lives in the REQUEST list;
  `reconcile_store` will then mirror it into the session file (harmless; on
  reload the full history is present and the brief re-derives).
- `read_file` content is either fully present or fully absent (memory
  992772cd/3541c0a6) — the brief references file PATHS only, never content.
- Server invariant: last message must be user (age_sweep comment, trim.rs:113)
  — the brief is never the last message (protected tail after it), so no issue.
