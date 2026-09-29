# Bug report — diff/edit tooling: backslash display doubling vs raw-byte edits (footgun)

- **Date:** 2026-07-08 (session implementing stats plan item 4c)
- **Component:** `read_file`, `search_content` (display) vs `apply_diff` / `replace_lines` (raw-byte edits)
- **Severity:** low (no data loss, no wrong writes) — but a real productivity footgun:
  ~6 failed edit attempts and a full re-diagnosis loop in one item.
- **Status:** reproduced & diagnosed; no fix yet (see Suggested fixes).

## Symptom

While editing Rust string literals containing escaped quotes (`\"`) or line-
continuation backslashes (`\` at EOL), `apply_diff` repeatedly failed with:

```
Block 1: search text not found
```

— even though the SEARCH text had been copied from the immediately preceding
`read_file` output. The same SEARCH text, entered "from memory" with the
correct raw bytes, matched on the first try.

Secondary confusion: the displayed text of a file containing `\"csv\"`
showed `\\\"csv\\\"`, making it look as if the file (or a previous edit)
contained double backslashes — several edits were then wasted "fixing"
backslashes that were fine.

## Controlled reproduction (probe file, 2026-07-08)

Probe: `target/backslash_probe.rs` (temp, deleted after diagnosis), written
with `write_file` from known content:

```rust
let a = "line with a line-continuation backslash \
         on the next line";
let b = "quotes inside: \"csv\" or \"json\"";
let c = "double backslash: \\ then a quote";
```

Results:

1. **`apply_diff` SEARCH with a trailing line-continuation `\`** copied
   exactly → **matches and applies** (no special handling needed).
2. **`apply_diff` SEARCH with `\"` escaped quotes** copied exactly →
   **matches and applies**.
3. **`apply_diff` REPLACE containing `\"` + trailing `\`** → the file ends
   up with exactly one backslash in each place. Verified by regex:
   - `search_content` pattern `new: \\"csv` (regex = one literal backslash +
     quote) → **1 match** in the edited line.
   - (A regex requiring backslash-backslash-quote → 0 matches.)
4. **`read_file` / `search_content` display** of that same line shows
   `\\\"csv\\\"` and a trailing `\\` — i.e. **every single backslash in the
   file is displayed doubled**, in both tools, consistently.

## Root cause (established)

- The edit tools operate on **raw file bytes** and do NOT mangle
  backslashes: what you send is what lands in the file, and SEARCH is an
  exact byte-for-byte match.
- The read/search tools **render each `\` as `\\`** in their output
  (likely escaping for transport/display). Nothing in their responses marks
  this, so text copied from their output carries doubled backslashes and no
  longer matches the raw file.

Net effect: **the tools' own output cannot be round-tripped into their own
SEARCH blocks** whenever the target text contains backslashes.

## Impact observed in the 4c session

- 4 failed `apply_diff` SEARCH attempts on `tools/builtin/improvement/metrics.rs`
  (tool description + schema param), each costing a re-read/re-think cycle.
- 1 false-positive "fix" pass (editing `\\\"` → `\"` in a file that already
  had `\"`) that had to be unwound.
- 1 misused tool call (`delete` on the repo root to remove a single line —
  survived, but shows how the confusion cascades).
- Total: ~30 min of the 4c item spent on backslash forensics.

## Workarounds that work

1. **Regex-verify before and after** edits to backslash-heavy lines:
   `search_content` with `regex: true`, where `\\` in the pattern = one
   literal backslash (e.g. `\\"csv` = backslash-quote-csv).
2. **Prefer `replace_lines`** for single backslash-heavy lines: line numbers
   come from read_file (unaffected by the doubling) and `verify_contains`
   should use **backslash-free** substrings.
3. **Anchor `apply_diff` SEARCH blocks on backslash-free text** (surrounding
   braces/identifiers); keep the backslash content in REPLACE only.
4. For Rust string literals: consider writing the new content as **one long
   line** (no line-continuation backslashes) when the edit replaces the
   whole literal.

## Suggested fixes (for the tool layer)

- **Preferred:** make `read_file` / `search_content` return raw bytes
  (no backslash doubling), or add an explicit `escaped_display: true` flag
  to their responses so agents know to un-escape before constructing SEARCH
  text.
- Alternative: have `apply_diff`'s "not found" error include the closest
  matching region (diff against the actual line) — the off-by-one-backslash
  case would then be visible immediately instead of opaque.
- Alternative: make SEARCH matching tolerant of doubled-backslash input
  (try the verbatim match first, then a single `\\`→`\` un-escape pass),
  and say in the success response when the tolerant path was used.

## Verification checklist (if fixed)

- [ ] `read_file` output for a file containing `\"` shows exactly one `\`.
- [ ] Text copied verbatim from `read_file` matches as an `apply_diff`
      SEARCH block (round-trip test).
- [ ] `apply_diff` "not found" errors point at the closest actual text.
