# Markdown support in the chat area

## Goal
AI assistant messages in the chat render as GitHub-flavored markdown (headings, bold/italic/strike,
inline code, fenced code blocks, lists, task lists, block quotes, tables, links, horizontal rules)
instead of raw text. User messages stay plain (fidelity of what the user typed).

## Design decisions
- **Parser**: `pulldown-cmark` 0.13 (`default-features = false`, options
  `ENABLE_TABLES | ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS`). Fast enough to re-parse per frame;
  no HTML needed.
- **Renderer**: custom egui renderer in a new module
  `wuffagent-egui/src/ui/chat_area/markdown.rs` (not the `egui_markdown` crate) so we can reuse
  the existing theme (`theme.code_bg`, `theme.accent`, ...) and the existing `code_block` frame
  helper from `tool_cards.rs`.
- **Inline model**: walk pulldown `Inline` events and build one `LayoutJob` per paragraph/heading
  with per-span `TextFormat`s:
  - bold → `FontFamily::from("Strong")`
  - italic → `TextFormat.italics`
  - strikethrough → `TextFormat.underline` (Strikethrough style)
  - inline code → monospace font + `TextFormat.bg_fill = theme.code_bg`
  - links → accent color + underline (v1: visual only, not clickable)
  - `wrap.break_anywhere = true` (long URLs/paths), same as `breaking_label`.
- **Blocks**:
  - Paragraph → wrapped LayoutJob.
  - Heading (1–6) → bold, size scale 17/16/15/14/13.5/13.5, small spacing.
  - Fenced code block → existing `code_block` frame; language shown as a small dim label when
    present; body monospace 11.5 inside a horizontal `ScrollArea` (no vertical clipping).
  - Blockquote → frame with a thick left accent-ish border (theme.divider), dim text.
  - List items → "•"/"◦"/"1." markers per depth, hanging indent via `ui.indent`; task items
    show ☐/☒.
  - Table → simple wrapping rows: header row bold + bottom divider, body rows with column
    separators; no fixed column widths (v1).
  - HR → 1px line in theme.divider.
  - Image → skipped (rare in LLM output); data-URI images could be a follow-up.
- **Scope**:
  - `MessageKind::Normal` assistant messages → markdown.
  - User messages → unchanged (plain white label).
  - Thinking messages → unchanged (plain dim italic).
  - Tool cards → unchanged.
  - Streaming line → stays plain text while streaming (re-parsing half-written fences each frame
    would flicker); the committed message renders as markdown (same text, minor reflow on commit
    is acceptable and already happens).
  - Edit mode (TextEdit) unchanged.
- **Copy button**: still copies the raw markdown source (no change).

## Phases
1. `cargo add pulldown-cmark` to `wuffagent-egui` (default-features = false).
2. New module `chat_area/markdown.rs`:
   - `draw_markdown(ui, text, size, color, theme)` entry point.
   - Inline event → LayoutJob builder with the style mapping above.
   - Block dispatch (paragraph, heading, code, blockquote, list, table, hr).
3. Wire into `bubbles.rs`: AI normal-message branch calls `draw_markdown` instead of
   `breaking_label`.
4. Tests (headless `egui::Context::default()`):
   - smoke: render a sample document exercising every block type without panicking;
   - unit: heading level → font size mapping; inline code span flags.
5. Verify: `cargo build` + `cargo test -p wuffagent-egui`; commit.

## Out of scope (follow-ups)
- Clickable links / opening URLs.
- Settings toggle "Render markdown in chat" (on by default).
- Markdown in tool-card summaries.
- Syntax highlighting of fenced code (plain monospace for now).

## Status
- [x] Plan
- [ ] Phase 1–2: dependency + renderer module
- [ ] Phase 3: wire into bubbles
- [ ] Phase 4: tests
- [ ] Phase 5: build + test + commit
