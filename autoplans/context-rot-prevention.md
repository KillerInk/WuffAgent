# Context-Rot Prevention After Trimming

Created: 2026-07-21. Status: **PLANNED** (not started).

Goal: stop the model from "going off the rails" after the context window is
reached and history is trimmed — it should keep the TASK, the user's
CORRECTIONS, the DECISIONS, and the CURRENT STATE, and know that compaction
happened.

## The rot gaps in the current pipeline (evidence)

`trim_messages` (wuffagent-core/src/trimming/summarizer/mod.rs:148) runs:
freshness pass (stale read_file pairs removed) → `age_sweep` (oldest-first
removal) → `summarize_old_tool_messages` (in-place tool-result summarization)
→ `truncate_largest_message` (halve → 41-char placeholder) → last-resort tail
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

- [ ] **S1 — Mission Brief v1 (deterministic).** `brief.rs` + wiring above.
      Unit tests: extraction (task/corrections/decisions/completed/files from a
      synthetic dropped span), merge + caps + never-drop-task, render marker,
      anchoring (brief survives a second trim, is replaced not stacked),
      first-user-message protection, no brief when nothing was dropped,
      idempotency. This is ~300-400 lines incl. tests; the biggest bang for the
      buck.
- [ ] **S2 — Measure rot.** New `MetricsLog::log_trim(agent, chars_before,
      chars_after, methods, brief_updated)` line, written from the agent loop
      where the trim runs (loop.rs:246). Later: improver/effect-check
      correlation — runs after a trim → verification outcome + tool-error rate
      vs. baseline (the I5 pattern), feeding the self-improvement loop.
- [ ] **S3 — Soften the cliff (config, no default behavior change).** Add
      `trim_trigger_pct` / `trim_target_pct` to `TrimConfig` (defaults 90/50 —
      today's proven values), read by `ChatClient::trim_trigger_chars` /
      `trim_target_chars`. Once S2 data exists, tune per agent (e.g. 90→65
      first cut, 50 hard floor) with evidence instead of guessing.
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
