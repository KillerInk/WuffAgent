# New `replace_lines` tool (line-based editing, alongside `apply_diff`)

## Motivation
`apply_diff` (SEARCH/REPLACE blocks) has been unreliable in practice:
- models fail to copy the SEARCH text *exactly* (indentation/whitespace drift) → "search text not found"
- ambiguity errors force extra read/retry round-trips
- long SEARCH payloads are fragile against truncated tool-call arguments

A line-based edit is easier for the model to produce: it already sees numbered
lines from `read_file` and only has to state WHICH lines and the NEW text.

## Decision
- **Keep `apply_diff`** (user decision: add the new tool, don't replace).
- Add builtin tool **`replace_lines`** in `wuffagent-core/src/tools/builtin/fileio/`.

## Tool contract
`replace_lines(path, start_line, end_line, new_content, verify_contains?)`

| Param | Type | Required | Meaning |
|---|---|---|---|
| `path` | string | yes | File to edit |
| `start_line` | integer | yes | First line to replace (1-indexed) |
| `end_line` | integer | yes | Last line to replace (1-indexed, inclusive) |
| `new_content` | string | yes | Replacement text; empty = pure deletion |
| `verify_contains` | string | no | Guard: if given, the targeted lines must contain this text (substring, LF-normalized); otherwise the tool fails and reports the actual targeted lines |

## Semantics (same robustness guarantees as apply_diff)
- Read via `read_text_file` (UTF-16 / non-UTF-8 → clear error, file untouched).
- BOM stripped before processing, restored on write.
- Dominant EOL (CRLF vs LF by majority) detected; line splitting via
  `str::lines()`; output rejoined with the file's EOL; original
  trailing-newline state preserved.
- Validation (file untouched on failure, error is self-correcting):
  - `start_line < 1` or `start_line > end_line` → error
  - `end_line > total_lines` → error with `total_lines` + snippet of the
    last ~8 lines (so the model can fix the numbers without re-reading)
  - empty file → error pointing to `write_file`
  - `verify_contains` mismatch → error showing the actual targeted lines
- Success JSON: `{path, start_line, end_line, lines_replaced,
  lines_inserted, verified, success}`.

## Code changes
1. `fileio/edits.rs` — `replace_lines()` op fn (next to `apply_diff`).
2. `fileio/mod.rs` — `ReplaceLinesTool` struct (name/schema/execute).
3. `tools/builtin/mod.rs` — register `replace_lines`.
4. `tools/preview.rs` — add `replace_lines` to the `path`-field preview list.
5. `trimming/filestate.rs` — add `replace_lines` to the mutating-tool list
   (stale read-file invalidation) + doc comment.
6. `trimming/brief.rs` — add to `FILE_MUTATING_TOOLS`.
7. `wuffagent-egui/src/ui/chat_area/mod.rs` — ✏️ icon list.
8. Tests: `fileio/tests/replace_lines.rs` (wired in `tests/mod.rs`):
   single-line, multi-line range, delete (empty), out-of-range end w/ snippet,
   start>end, empty file, verify pass + verify mismatch, CRLF preserved,
   BOM preserved, LF stays LF, CRLF payload on LF file normalized,
   no-trailing-newline preserved, tool wiring test.

## Out of scope
- Insertion at a position with no existing line (start > end): use
  `append_file` / `write_file`, or replace line N with "new + line N".
  (Revisit if models keep needing it.)
- Removing `apply_diff` (kept on purpose).

## Verification
`cargo test -p wuffagent-core` (fileio + trimming suites green), then
self-restart to load the new binary.
