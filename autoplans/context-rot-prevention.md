# Context-Rot Prevention After Trimming

Created: 2026-09-27. Status: **S1+S2+S3 DONE** (2026-09-27), **S4a DONE** (2026-09-28, session_note tool + anchored notes), **S4b DONE** (2026-09-27, LLM brief polish, 1537247) — all S1–S4b on master as of 1913907. S5 (pre-trim note update) sketched below, pending.

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
- [x] **S4 — Stretch: agent-side scratchpad + LLM polish.** (a) DONE 2026-09-28; (b) DONE 2026-09-27 (1537247, as-built notes below)
      a) `session_note` tool: the agent explicitly records "state / next step";
         the loop re-injects the note right after the system prompt on every
         call (capped); the brief merges it with top priority. Strongest
         guarantee — survives ANY trim. **DONE 2026-09-28 — detailed design +
         as-built notes below.**
      b) Optional LLM brief polish behind a flag: when a dropped span is large
         and a client is available, refine the brief (request carries ONLY the
         dropped span + old brief — small, cannot overflow); deterministic
         extraction is always the fallback.

      ### S4a detailed design (session_note)

      **Shape: anchored state message, not a re-injected copy.** The note is a
      real user-role message `[SESSION NOTE — <note>]` inserted ONCE at the
      anchor slot (right after the system prompt, BEFORE the task) and is
      PROTECTED from trimming exactly like the task: `age_sweep` and
      `truncate_largest_message` skip it. "Re-injected on every call" is
      satisfied structurally (it is in every request list) plus a belt-and-
      braces `reanchor_notes` pass at each loop boundary that moves a drifted
      note (e.g. after a session reload, where store append-order puts it at
      the END of the list) back into the anchor slot before the LLM call.

      **Capped + folded into the brief (top priority):** max 3 anchored note
      messages (`NOTES_MAX_ITEMS`). Adding a 4th evicts the OLDEST note: its
      content is folded into the session brief's new `Notes:` section FIRST
      (the brief is rendered + (re)inserted in place even though no trim is
      happening), then the note message is removed. So a note's state is
      carried either as the verbatim anchored message or inside the brief —
      never both lost. `SessionBrief.notes: Vec<String>` (cap 400 chars per
      line, newest kept; evicted LAST by `drop_oldest`, after files — agent-
      authored state outranks derivable file lists). Render/parse contract
      gains a `Notes:` section (between `In progress:` and `Decisions:`).

      **Task disambiguation (the one real bug risk):** with a note at index 1,
      "the first user message" is the note, not the task. Every "first user
      message = task" site must mean "first user message that is neither a
      brief nor a note": `age_sweep`'s task guard, `trim_messages`'
      `task_seed`, and `apply_brief`'s anchor (skip the note block, then the
      task). `merge_dropped` folds a dropped note message into `brief.notes`
      (belt-and-braces; notes are protected so the sweep cannot drop them,
      but the cap-evicted note still reaches the brief via the fold path).

      **Wiring:** `SessionNoteRequest { note }` (agents/types.rs);
      `SessionNoteTool` with its own per-execution mailbox (same pattern as
      `hand_back`); injected in `AgentBuilder::build` exactly when
      `AgentConfig.session_note_enabled` (new flag, serde default TRUE —
      protective feature, opt-out per agent; set in all three parse sites).
      At the top of `run_llm_loop` (after `drain_injections`): take pending
      note → `brief::apply_note(messages, &note)` → `record_in_store` the
      anchored message (persisted + visible in the transcript — the user can
      see what the agent recorded) → `reanchor_notes(messages)`. The
      `allowed_tools` filter must let `session_note` through like
      `shell`/`handoff`/`restart` when the flag is on. System prompt gains a
      short `## SESSION NOTE` section when enabled (when to call it: durable
      state / decisions / what's-next, especially before long tool-heavy
      steps; keep it short and self-contained). Caps: tool input truncated at
      2000 chars (`NOTE_INPUT_MAX`); brief line cap 400 (`NOTE_MAX`).

      **Store/reload semantics:** `reconcile_store` (post-trim) replaces the
      store with the request list's storable projection, so after any trim the
      store matches the anchored layout exactly. Between note-apply and the
      next trim the store still holds the note at its append position (and
      any cap-evicted note message) — on reload that is strictly MORE state,
      and `reanchor_notes` re-anchors before the first LLM call. No state can
      be lost: a note is in the request list (verbatim) or in the brief
      (folded) or in the store (pre-reconcile) at all times.

      **Metrics:** no new line kind for now (S2's Trim lines already capture
      the rot signal; note usage is visible in the transcript + brief). Add a
      `note` line only if the improver needs the evidence later.

      **Tests:** brief.rs (note roundtrip through render/parse; merge_dropped
      folds a dropped note into `notes`, not corrections/task; drop_oldest
      evicts notes last; apply_note: insert-at-anchor, dedupe, cap-eviction
      folds oldest into the brief + removes the message; reanchor_notes moves
      a tail note to the anchor); summarizer tests (a note survives a big
      trim untouched; the task guard still protects the REAL task with a note
      at index 1); session_note.rs (mailbox write, merge of two pending,
      empty no-op, input truncation).

### S4a as-built notes (2026-09-28)

- All design points landed as written: `brief.rs` gained
  `SessionBrief.notes` + `NOTE_MARKER` + `is_note_message` / `note_content` /
  `apply_note` / `reanchor_notes` (max 3 anchored notes, cap-fold into the
  brief's `Notes:` section); the summarizer extraction skips note messages;
  `trim_messages` / `truncate_largest_message` / the first-user-task scan all
  treat "the task" as the first user message that is neither brief nor note;
  `SessionNoteRequest` (agents/types.rs) + `SessionNoteTool` (per-execution
  mailbox, `with_session_note_tool` rebuild in tools/manager.rs) injected in
  `Agent::builder` when `session_note_enabled` (serde default true; all legacy
  parse sites set it true); the loop drains the mailbox after
  `drain_injections`, `apply_note` + `record_in_store` (the note is a REAL
  user message in the transcript and the store, so it survives reloads), then
  `reanchor_notes` every round (fixes the post-reload drift where the store's
  append order leaves notes at the end); the allowlist push and the
  `## SESSION NOTE` prompt block are both gated on the flag; tool input is
  truncated at `NOTE_INPUT_MAX` (2000 chars) in the loop (`truncate_note`).
- Bug found while testing: `note_content` left the closing `]` in the note
  text (render is `{NOTE_MARKER}{note}]`, the impl only stripped the prefix),
  so folded brief lines read `note 0]`. Fixed: strip the trailing `]` (only
  the last char — a note whose own text ends in `]` still round-trips).
  Regression test added.
- Tests as planned: 7 new brief.rs note tests (anchor slot, dedupe, cap-fold,
  reanchor drift/idempotent/no-system, note_content bracket edge, task-scan
  disambiguation) + the existing render/parse roundtrip now carries a note;
  6 session_note.rs tool tests (mailbox write, second-pending error,
  empty/missing, schema).
- Deviation (intentional): the mailbox is created for EVERY agent and only the
  TOOL injection is flag-gated, so `Agent.session_note_mailbox` stays a plain
  `Arc` (no `Option` plumbing); with the flag off the drain is a no-op and
  the tool is simply absent from the registry.
- Not covered (deferred): no agent-level loop test that a pending note
  appears in the NEXT LLM request (would need a scripted LLM harness in
  tests/context.rs style); covered indirectly by the apply_note/reanchor
  unit tests + the tool tests. No `note` metrics line (per the design).
- Known cosmetic gap (observed during the 2026-09-28 dogfood): the note
  message is `record_in_store`d but emits no UI event, so it does not
  appear as a bubble in the transcript view until the next session
  flush/reload. It IS present in every LLM request (the primary purpose)
  and in the store. If it matters, emit an `AppEvent` for it in the loop
  drain.
- Live dogfood (2026-09-28, relaunch build from 1e3ea23): `session_note`
  call returned `session_note_queued`; marker expected in the session file
  at the next flush. Tool description cap wording fixed in e091a8a (input
  cap 2000; 400 only on brief-fold).

### S4b as-built notes (2026-09-27)

- `TrimConfig.llm_brief_polish` (types/policy.rs, serde default **true** —
  protective feature, per-agent opt-out; missing-field test extended in
  trimming/config/tests.rs).
- `ContextTrimming::trim_messages_detailed(messages, target, config) ->
  (usize, Vec<Message>)` (summarizer/mod.rs): the old `trim_messages` body,
  now also returning the dropped span in chronological order (exactly what
  the age sweeps removed; freshness-pass removals are NOT in the span).
  `trim_messages` is a thin wrapper (`.0`), so all existing call sites and
  tests are untouched.
- `brief.rs` gains: `BRIEF_POLISH_MIN_DROPPED_CHARS` (4_000),
  `POLISH_SPAN_MAX_CHARS` (12_000), `parse_polish` (same section grammar as
  `from_rendered` minus the marker requirement — tolerant of model wrapper
  text), `polish_request(prev_brief, dropped) -> [system, user]` (system
  prompt states the merge rules + the exact section format + "use ONLY facts
  present ... never invent"), `polish_span_text` (one line per dropped
  message, tool-call names appended, oldest-first eviction at the cap,
  previous brief renders excluded — carried as OLD BRIEF instead).
- Loop wiring (agents/agent/loop.rs, the PROACTIVE trim site only):
  `trim_messages_detailed` → when `llm_brief_polish` and the span is ≥
  `BRIEF_POLISH_MIN_DROPPED_CHARS` chars, `Agent::polish_brief` makes ONE
  non-streaming `complete_messages` call (no tools). Response is re-validated
  through the deterministic contract: `parse_polish` → task restored from the
  deterministic brief when the model dropped it → `enforce_total_cap` →
  `render` → fit-guard (`rendered < target_chars`) → `apply_brief` (in-place
  update, never stacked). ANY failure (LLM error, unparseable, over cap)
  keeps the deterministic brief — it is always the fallback. Runs before
  `reconcile_store`, so the polished brief is what the session file gets.
  Log: `[AGENT] Agent '<name>' brief polished by the LLM (N chars)`.
- Tests: 5 new brief.rs (request carries only brief+span; span cap keeps
  newest + marks omitted; span excludes a previous brief; parse_polish
  roundtrip without marker; garbage rejected) + 2 new summarizer/tests/trim.rs
  (detailed returns the exact dropped span chronological, no drop/keep
  overlap, net shrink = removed - briefs re-inserted; wrapper and detailed
  agree on count and final list). Suite: 739 passed, 0 failed.
- Not covered (deferred): the backstop (overflow-retry) trim site does not
  polish (the emergency path should not pay for an extra round-trip); no
  loop-level test of the polish call itself (needs a scripted LLM harness);
   the polish request uses the agent's configured reasoning_effort.

### S5 — Pre-trim session-note update (sketch, pending)

Problem: the anchored session note is the strongest rot guarantee, but it is
only as fresh as the agent's LAST `session_note` call. When the proactive trim
fires in the middle of a long tool-heavy stretch, the newest state sits in the
about-to-be-dropped tail — the brief captures it deterministically, but the
note the model reads first (before the task) stays stale.

Design (deterministic, synchronous, no extra LLM round):
- At the PROACTIVE trim site (agents/agent/loop.rs, after
  `trim_messages_detailed` returns `dropped`), when `session_note_enabled`:
  extract the fresh state line from the dropped span (reuse the brief's
  `in_progress`/last-assistant heuristic) and, when it differs from the
  current note, refresh the note via `brief::apply_note` with
  `<existing note> || <state line as of this compaction>` (capped at
  NOTE_MAX on fold / NOTE_INPUT_MAX on store). `apply_note` dedupe keeps it
  idempotent; the >3 cap folds the oldest note into the brief's `Notes:`
  section, so no state is lost.
- Runs BEFORE `reconcile_store` so the refreshed note lands in the session
  file; record it in the store like the tool path does (visible in the
  transcript).
- The overflow backstop site (loop.rs ~line 515) does NOT refresh the note
  (emergency path, same rationale as the polish skip).
- Metrics: piggyback on the existing Trim line (no new kind); log
  `[AGENT] Agent '<name>' session note refreshed pre-trim`.
- Tests: brief.rs (refresh dedupe/idempotent, cap-fold on repeated trims),
  loop-level unit via the existing scripted harness if available, else
  covered by the brief unit tests.

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
