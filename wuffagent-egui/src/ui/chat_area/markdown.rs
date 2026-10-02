//! Markdown rendering for chat messages.
//!
//! Parses GFM-flavoured markdown (pulldown-cmark: tables, strikethrough,
//! task lists) and renders it into the existing chat theme. Used for AI
//! messages in `bubbles.rs`; user messages stay plain text. Thinking
//! blocks also render as markdown, but dimmed (`dim: true`): regular
//! weight, dimmed links and code, so they read as quieter than the
//! answer they belong to.
//!
//! Supported elements:
//! - inline: **bold** (real bold via the "Strong" font family, see
//!   `fonts.rs`), *italic*, ~~strikethrough~~, `inline code`,
//!   [links](...) (accent colour + underline)
//! - headings (# … ######), block quotes (left bar), horizontal rules
//! - lists: bullets (•/◦/▪ by depth), ordered (numbered, restarts per
//!   list), task lists (☑/☐ as the bullet)
//! - fenced code blocks: the tool-card code frame (theme.code_bg),
//!   monospace, horizontal scroll for long lines, language label
//! - tables: equal-width columns, bold header row, divider under the head

use std::collections::VecDeque;

use eframe::egui;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::ui::state::ChatApp;
use crate::ui::theme::Theme;

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Owned (`'static`) parsed markdown events, so they can be cached in the
/// egui Context without borrowing the source text.
type OwnedEvents = Vec<Event<'static>>;

/// Per-Context cache of parsed markdown (egui temp data — cleared with the
/// context). Immediate mode redraws the whole transcript every frame, but a
/// committed message's text never changes, so parse it once and reuse the
/// events. Without this, every frame re-ran the pulldown-cmark parse for
/// every AI/thinking message (plus the growing stream buffer per token).
#[derive(Clone, Default)]
struct MarkdownParseCache {
    /// Key: (64-bit hash of the text, byte length) -> owned events.
    entries: std::collections::HashMap<(u64, u32), std::sync::Arc<OwnedEvents>>,
}

/// Cache bound in entries. On overflow the whole map is dropped — the
/// visible session's messages re-parse once, and memory stays bounded even
/// across many large sessions.
const MAX_PARSE_CACHE_ENTRIES: usize = 256;

/// Cache key for a text body: 64-bit hash + byte length (the length reduces
/// the already-negligible collision risk).
fn text_key(text: &str) -> (u64, u32) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    (h.finish(), text.len() as u32)
}

/// Parsed events for `text`: a cache hit is an O(1) `Arc::clone`, a miss
/// parses once (and stores it when `cache` is true).
fn parsed_events(ctx: &egui::Context, text: &str, cache: bool) -> std::sync::Arc<OwnedEvents> {
    let key = text_key(text);
    // Raw read (no clone of the whole map): type-keyed at `Id::NULL`.
    let cached = ctx.data(|d| {
        d.get_temp_raw(egui::util::id_type_map::RawKey::new::<MarkdownParseCache>(
            egui::Id::NULL,
        ))
        .and_then(|v| v.downcast_ref::<MarkdownParseCache>())
        .and_then(|c| c.entries.get(&key))
        .cloned()
    });
    if let Some(hit) = cached {
        return hit;
    }
    let events = Parser::new_ext(text, options())
        .map(Event::into_static)
        .collect::<OwnedEvents>();
    let arc = std::sync::Arc::new(events);
    if cache {
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<MarkdownParseCache>(egui::Id::NULL);
            if cache.entries.len() >= MAX_PARSE_CACHE_ENTRIES {
                cache.entries.clear();
            }
            cache.entries.insert(key, arc.clone());
        });
    }
    arc
}

/// Font family used for bold runs and headings. Registered in `fonts.rs`
/// (Segoe UI Semibold on Windows, Arial Bold on macOS; when the file is
/// missing it falls back to the regular proportional fonts).
const STRONG_FAMILY: &str = "Strong";

/// Vertical extent used when building a `max_rect` for a nested content
/// scope (list item, table cell) that should "grow to fit its content".
///
/// A large FINITE value rather than `f32::INFINITY`: egui's cursor can hold
/// `±inf` as a "fill in later" placeholder, and `Rect::from_min_size` does
/// `min + size`, so `min.y == -inf` with an `inf` height yields `NaN` and
/// trips egui's `max_rect is NaN` debug-assert. A finite cap never produces
/// NaN, and no real list item / table cell comes close to it.
const VERTICAL_GROW_CAP: f32 = 1_000_000.0;

/// Nesting depth of inline styles (nested markup can push these above 1).
#[derive(Clone, Copy, Default)]
struct InlineStyle {
    bold: u32,
    italics: u32,
    strike: u32,
    link: u32,
}

/// One level of an open list (for numbering + bullet depth).
struct ListInfo {
    ordered: bool,
    next: u64,
}

/// Render markdown `text` into a chat bubble (AI messages only).
pub(super) fn draw_markdown(
    ui: &mut egui::Ui,
    text: &str,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
) {
    draw_markdown_impl(ui, text, size, color, theme, false, true);
}

/// Render markdown `text` with an explicit dim flag: `dim = true` renders
/// everything in `color` with regular weight — no bold "Strong" font, dimmed
/// links and inline code — used for Thinking blocks.
pub(super) fn draw_markdown_dimmed(
    ui: &mut egui::Ui,
    text: &str,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
    dim: bool,
) {
    draw_markdown_impl(ui, text, size, color, theme, dim, true);
}

/// Streaming variant of [`draw_markdown_dimmed`]: the same rendering, but the
/// parse is NOT inserted into the cache — the live buffer changes every
/// frame (per token), so caching it would add a fresh entry every frame and
/// evict the committed messages' entries within seconds.
pub(super) fn draw_markdown_streaming(
    ui: &mut egui::Ui,
    text: &str,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
    dim: bool,
) {
    draw_markdown_impl(ui, text, size, color, theme, dim, false);
}

fn draw_markdown_impl(
    ui: &mut egui::Ui,
    text: &str,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
    dim: bool,
    cache: bool,
) {
    let events = parsed_events(ui.ctx(), text, cache);
    // Borrow the cached events — a cursor over the shared `Vec` instead of a
    // per-frame deep clone of the whole event vector (every clone is a heap
    // allocation per text/code event, for every message, every frame).
    let mut cursor = 0usize;
    let mut lists = Vec::new();
    let mut queue: VecDeque<&Event> = VecDeque::new();
    draw_blocks(
        &*events,
        &mut cursor,
        ui,
        theme,
        size,
        color,
        &mut lists,
        &mut queue,
        false,
        dim,
    );
}

fn heading_font_size(level: HeadingLevel) -> f32 {
    match level {
        HeadingLevel::H1 => 17.0,
        HeadingLevel::H2 => 16.0,
        HeadingLevel::H3 => 15.0,
        HeadingLevel::H4 => 14.0,
        HeadingLevel::H5 | HeadingLevel::H6 => 13.5,
    }
}

fn new_job() -> egui::epaint::text::LayoutJob {
    let mut job = egui::epaint::text::LayoutJob::default();
    job.wrap.break_anywhere = true;
    job
}

fn flush_job(job: &mut Option<egui::epaint::text::LayoutJob>, ui: &mut egui::Ui) {
    if let Some(j) = job.take() {
        ui.add(egui::Label::new(j).wrap());
    }
}

/// Render the stream of block-level events until the event iterator/queue
/// runs out.
/// `queue` holds events the caller already pulled (e.g. an item's first
/// inline event when sniffing for a task-list marker).
/// `in_item`: when true, stop at the current list item's boundary
/// (`End(Item)`) so the item's content cannot swallow sibling items and
/// later blocks (they would be drawn inside the item's scope).
/// `dim`: render everything in `color` with regular weight (Thinking blocks).
/// Take the next event from the borrowed parsed-event slice (cursor-based,
/// so the shared events are never cloned per frame).
#[inline]
fn next_ev<'it, 'a>(
    events: &'it [Event<'a>],
    cursor: &mut usize,
) -> Option<&'it Event<'a>> {
    let i = *cursor;
    if i < events.len() {
        *cursor += 1;
        Some(&events[i])
    } else {
        None
    }
}

fn draw_blocks<'a, 'it>(
    events: &'it [Event<'a>],
    cursor: &mut usize,
    ui: &mut egui::Ui,
    theme: &Theme,
    size: f32,
    color: egui::Color32,
    lists: &mut Vec<ListInfo>,
    queue: &mut VecDeque<&'it Event<'a>>,
    in_item: bool,
    dim: bool,
) {
    let mut job: Option<egui::epaint::text::LayoutJob> = None;
    let mut style = InlineStyle::default();

    loop {
        let Some(event) = queue
            .pop_front()
            .or_else(|| next_ev(events, cursor))
        else {
            break;
        };
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    flush_job(&mut job, ui);
                    job = Some(new_job());
                }
                Tag::Heading { level, .. } => {
                    flush_job(&mut job, ui);
                    ui.add_space(6.0);
                    let hsize = heading_font_size(*level);
                    let mut hj = new_job();
                    let mut hs = InlineStyle {
                        bold: 1,
                        ..Default::default()
                    };
                    let mut done = false;
                    while !done {
                        match queue.pop_front().or_else(|| next_ev(events, cursor)) {
                            Some(Event::End(TagEnd::Heading(_))) | None => done = true,
                            Some(ev) => apply_inline_event(
                                &mut hj,
                                ev,
                                &mut hs,
                                hsize,
                                theme.text_primary,
                                theme,
                                dim,
                            ),
                        }
                    }
                    ui.add(egui::Label::new(hj).wrap());
                    ui.add_space(4.0);
                }
                Tag::CodeBlock(kind) => {
                    flush_job(&mut job, ui);
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    let mut buf = String::new();
                    let mut done = false;
                    while !done {
                        match queue.pop_front().or_else(|| next_ev(events, cursor)) {
                            Some(Event::Text(t)) => buf.push_str(t),
                            Some(Event::End(TagEnd::CodeBlock)) | None => done = true,
                            _ => {}
                        }
                    }
                    render_code_block(ui, theme, &lang, &buf);
                    ui.add_space(4.0);
                }
                Tag::BlockQuote(_) => {
                    flush_job(&mut job, ui);
                    ui.add_space(4.0);
                    let inner = ui.indent(ui.id().with("md_quote"), |ui| {
                        draw_blocks(
                            events,
                            cursor,
                            ui,
                            theme,
                            size - 0.5,
                            theme.text_secondary,
                            lists,
                            queue,
                            in_item,
                            dim,
                        );
                    });
                    let r = inner.response.rect;
                    ui.painter().line_segment(
                        [
                            egui::pos2(r.min.x - 4.0, r.min.y + 1.0),
                            egui::pos2(r.min.x - 4.0, r.max.y - 1.0),
                        ],
                        egui::Stroke::new(2.0, theme.divider),
                    );
                    ui.add_space(4.0);
                }
                Tag::List(start) => {
                    flush_job(&mut job, ui);
                    ui.add_space(3.0);
                    lists.push(ListInfo {
                        ordered: start.is_some(),
                        next: start.unwrap_or(1),
                    });
                }
                Tag::Item => {
                    draw_list_item(
                        events,
                        cursor,
                        ui,
                        theme,
                        size,
                        color,
                        lists,
                        queue,
                        dim,
                    )
                }
                Tag::Table(_) => {
                    flush_job(&mut job, ui);
                    ui.add_space(4.0);
                    render_table(events, cursor, ui, theme, size, color, queue, dim);
                    ui.add_space(4.0);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush_job(&mut job, ui);
                    ui.add_space(4.0);
                }
                // Item content is drawn inside the item's own scope; stop
                // here so the next item / following blocks stay outside.
                TagEnd::Item if in_item => break,
                TagEnd::List(_) => {
                    lists.pop();
                    ui.add_space(3.0);
                }
                _ => {}
            },
            Event::Rule => {
                flush_job(&mut job, ui);
                ui.add_space(3.0);
                let c = ui.cursor().min;
                ui.painter().line_segment(
                    [
                        egui::pos2(c.x, c.y),
                        egui::pos2(c.x + ui.available_width(), c.y),
                    ],
                    egui::Stroke::new(1.0, theme.divider),
                );
                ui.allocate_space(egui::vec2(ui.available_width(), 1.0));
                ui.add_space(3.0);
            }
            // Inline content: append to the open paragraph job (or a
            // fresh one for stray text outside a block).
            event => {
                let j = job.get_or_insert_with(new_job);
                apply_inline_event(j, event, &mut style, size, color, theme, dim);
            }
        }
    }
    flush_job(&mut job, ui);
}

/// One list item: marker (bullet / number / task checkbox) + content.
fn draw_list_item<'a, 'it>(
    events: &'it [Event<'a>],
    cursor: &mut usize,
    ui: &mut egui::Ui,
    theme: &Theme,
    size: f32,
    color: egui::Color32,
    lists: &mut Vec<ListInfo>,
    queue: &mut VecDeque<&'it Event<'a>>,
    dim: bool,
) {
    ui.add_space(2.0);

    // If the item's first inline event is a TaskListMarker, pull it out so
    // the checkbox can replace the bullet.
    let mut task: Option<bool> = None;
    if let Some(e1) = queue.pop_front().or_else(|| next_ev(events, cursor)) {
        match e1 {
            // Keep the borrowed event to requeue below (no clone).
            ev @ Event::Start(Tag::Paragraph) => {
                if let Some(e2) = queue.pop_front().or_else(|| next_ev(events, cursor)) {
                    match e2 {
                        Event::TaskListMarker(b) => task = Some(*b),
                        other => queue.push_front(other),
                    }
                }
                queue.push_front(ev);
            }
            other => queue.push_front(other),
        }
    }

    let marker = match task {
        Some(true) => "☑".to_string(),
        Some(false) => "☐".to_string(),
        None => match lists.last() {
            Some(li) if li.ordered => format!("{}. ", li.next),
            _ => ["•", "◦", "▪"][lists.len().saturating_sub(1).min(2)].to_string(),
        },
    };
    if let Some(li) = lists.last_mut() {
        if li.ordered {
            li.next += 1;
        }
    }

    let mcolor = match task {
        Some(true) => theme.success,
        _ => theme.text_dim,
    };
    ui.horizontal(|inner| {
        inner.spacing_mut().item_spacing.x = 5.0;
        inner.add(egui::Label::new(
            egui::RichText::new(&marker).color(mcolor).size(size - 1.0),
        ));
        // Vertical scope with an exact width: a plain `scope` would inherit
        // the row's HORIZONTAL layout, letting the item's content
        // (paragraphs, nested lists, code blocks, tables) flow
        // left-to-right inside the item instead of stacking under it.
        let w = inner.available_width().max(20.0);
        inner.scope_builder(
            egui::UiBuilder::new()
                .layout(egui::Layout::top_down_justified(egui::Align::LEFT))
                .max_rect(egui::Rect::from_min_size(
                    inner.cursor().min,
                    egui::vec2(w, VERTICAL_GROW_CAP),
                )),
            |v| draw_blocks(events, cursor, v, theme, size, color, lists, queue, true, dim),
        );
    });
    ui.add_space(2.0);
}

/// Collect a table (header + rows of inline jobs) and draw it with
/// equal-width columns.
fn render_table<'a, 'it>(
    events: &'it [Event<'a>],
    cursor: &mut usize,
    ui: &mut egui::Ui,
    theme: &Theme,
    size: f32,
    color: egui::Color32,
    queue: &mut VecDeque<&'it Event<'a>>,
    dim: bool,
) {
    let mut header: Vec<egui::epaint::text::LayoutJob> = Vec::new();
    let mut rows: Vec<Vec<egui::epaint::text::LayoutJob>> = Vec::new();
    let mut in_head = false;
    let mut row: Option<Vec<egui::epaint::text::LayoutJob>> = None;
    let mut cell: Option<egui::epaint::text::LayoutJob> = None;
    let mut cell_style = InlineStyle::default();

    loop {
        let Some(ev) = queue
            .pop_front()
            .or_else(|| next_ev(events, cursor))
        else {
            break;
        };
        match ev {
            Event::Start(Tag::TableHead) => in_head = true,
            Event::End(TagEnd::TableHead) => in_head = false,
            Event::Start(Tag::TableRow) => row = Some(Vec::new()),
            Event::Start(Tag::TableCell) => {
                cell = Some(new_job());
                // No bold for dim (thinking) tables: regular weight throughout.
                cell_style = if in_head && !dim {
                    InlineStyle {
                        bold: 1,
                        ..Default::default()
                    }
                } else {
                    InlineStyle::default()
                };
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(c) = cell.take() {
                    if let Some(r) = row.as_mut() {
                        r.push(c);
                    }
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(r) = row.take() {
                    if in_head {
                        header = r;
                    } else {
                        rows.push(r);
                    }
                }
            }
            Event::End(TagEnd::Table) => break,
            ev => {
                if let Some(c) = cell.as_mut() {
                    apply_inline_event(c, ev, &mut cell_style, size, color, theme, dim);
                }
            }
        }
    }

    if header.is_empty() && rows.is_empty() {
        return;
    }
    let n = header
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(1))
        .max(1);
    // Subtract the between-cell spacing: a row is n*col_w + (n-1)*spacing
    // wide, so without this it exceeds the column by (n-2)*spacing.
    let spacing = 8.0;
    let col_w = ((ui.available_width() - (n as f32 - 1.0) * spacing) / n as f32).max(48.0);

    let draw_row = |ui: &mut egui::Ui, cells: &[egui::epaint::text::LayoutJob]| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for c in cells {
                let mut cc = c.clone();
                cc.wrap.break_anywhere = true;
                // Explicit max_rect: `set_width` only extends, never shrinks,
                // so a scope inheriting the row's full width would wrap at
                // that width and push the following cells past the column.
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(col_w, VERTICAL_GROW_CAP),
                    )),
                    |ui| ui.add(egui::Label::new(cc).wrap()),
                );
            }
        });
        ui.add_space(2.0);
    };
    if !header.is_empty() {
        draw_row(ui, &header);
        let c = ui.cursor().min;
        ui.painter().line_segment(
            [
                egui::pos2(c.x, c.y - 2.0),
                egui::pos2(c.x + ui.available_width(), c.y - 2.0),
            ],
            egui::Stroke::new(1.0, theme.divider),
        );
        ui.add_space(2.0);
    }
    for r in &rows {
        draw_row(ui, r);
    }
}

/// Themed code block: the tool-card code frame, language label, and a
/// horizontal scroll area so long lines scroll instead of wrapping.
fn render_code_block(ui: &mut egui::Ui, theme: &Theme, lang: &str, code: &str) {
    if code.trim().is_empty() && lang.is_empty() {
        return;
    }
    ChatApp::code_block(ui, theme, |ui| {
        if !lang.is_empty() {
            ui.label(egui::RichText::new(lang).color(theme.text_dim).size(9.5));
            ui.add_space(3.0);
        }
        let code = code.trim_end_matches(['\r', '\n']);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hasher::write(&mut hasher, code.as_bytes());
        std::hash::Hasher::write(&mut hasher, lang.as_bytes());
        let id = egui::Id::new(&format!("mdcode_{:x}", std::hash::Hasher::finish(&hasher)));
        egui::ScrollArea::horizontal()
            .id_salt(id)
            .show(ui, |ui| {
                for line in code.lines() {
                    ui.label(
                        egui::RichText::new(line)
                            .monospace()
                            .size(11.5)
                            .color(theme.code_text),
                    );
                }
            });
    });
}

/// Apply one inline event to a layout job (text, code span, breaks,
/// emphasis toggles).
fn apply_inline_event(
    job: &mut egui::epaint::text::LayoutJob,
    event: &Event,
    style: &mut InlineStyle,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
    dim: bool,
) {
    match event {
        Event::Text(t) => append_inline(job, t, *style, size, color, theme, false, dim),
        Event::Code(c) => append_inline(job, c, *style, size, color, theme, true, dim),
        Event::SoftBreak => append_plain(job, " ", size, color),
        Event::HardBreak => append_plain(job, "\n", size, color),
        Event::Start(Tag::Emphasis) => style.italics += 1,
        Event::End(TagEnd::Emphasis) => style.italics = style.italics.saturating_sub(1),
        Event::Start(Tag::Strong) => style.bold += 1,
        Event::End(TagEnd::Strong) => style.bold = style.bold.saturating_sub(1),
        Event::Start(Tag::Strikethrough) => style.strike += 1,
        Event::End(TagEnd::Strikethrough) => style.strike = style.strike.saturating_sub(1),
        Event::Start(Tag::Link { .. }) => style.link += 1,
        Event::End(TagEnd::Link) => style.link = style.link.saturating_sub(1),
        // Task markers normally surface as the list bullet; if one shows up
        // here (outside an item) render it inline.
        Event::TaskListMarker(checked) => {
            append_plain(job, if *checked { "☑ " } else { "☐ " }, size, color);
        }
        Event::Html(_) | Event::InlineHtml(_) => {} // skip raw HTML
        _ => {}
    }
}

fn append_plain(
    job: &mut egui::epaint::text::LayoutJob,
    text: &str,
    size: f32,
    color: egui::Color32,
) {
    job.append(
        text,
        0.0,
        egui::epaint::text::TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            ..Default::default()
        },
    );
}
/// Append a text run with the current inline style (bold via the "Strong"
/// family, italics, strikethrough, links; `code` = inline code span).
/// `dim = true` (Thinking blocks): regular weight, links keep `color`,
/// inline code uses `color` with no background.
fn append_inline(
    job: &mut egui::epaint::text::LayoutJob,
    text: &str,
    style: InlineStyle,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
    code: bool,
    dim: bool,
) {
    let (font_id, color, background, expand_bg) = if code {
        if dim {
            (egui::FontId::monospace(size - 0.5), color, egui::Color32::TRANSPARENT, 0.0)
        } else {
            (
                egui::FontId::monospace(size - 0.5),
                theme.code_text,
                theme.code_bg,
                1.0,
            )
        }
    } else {
        let font_id = if style.bold > 0 && !dim {
            egui::FontId::new(size, egui::FontFamily::Name(STRONG_FAMILY.into()))
        } else {
            egui::FontId::proportional(size)
        };
        let color = if style.link > 0 && !dim { theme.accent } else { color };
        (font_id, color, egui::Color32::TRANSPARENT, 0.0)
    };
    job.append(
        text,
        0.0,
        egui::epaint::text::TextFormat {
            font_id,
            italics: style.italics > 0,
            color,
            underline: if style.link > 0 {
                egui::Stroke::new(1.0, theme.accent)
            } else {
                egui::Stroke::NONE
            },
            strikethrough: if style.strike > 0 {
                egui::Stroke::new(1.0, color)
            } else {
                egui::Stroke::NONE
            },
            background,
            expand_bg,
            ..Default::default()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    /// Render `text` through [`draw_markdown`] in a headless egui context and
    /// collect (text, height) for every produced text shape (galley).
    fn rendered(text: &str) -> Vec<(String, f32)> {
        rendered_dimmed(text, false)
    }

    /// Like `rendered`, but with an explicit dim flag (Thinking blocks).
    fn rendered_dimmed(text: &str, dim: bool) -> Vec<(String, f32)> {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::emoji_fonts());
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_width(560.0);
            draw_markdown_dimmed(
                ui,
                text,
                13.5,
                egui::Color32::from_rgb(235, 238, 242),
                &Theme::dark(),
                dim,
            );
        });
        let mut out_shapes = Vec::new();
        for shape in &out.shapes {
            if let egui::epaint::ClippedShape {
                shape: egui::Shape::Text(ts),
                ..
            } = shape
            {
                out_shapes.push((ts.galley.job.text.clone(), ts.galley.rect.height()));
            }
        }
        out.drop_without_applying_deltas();
        out_shapes
    }

    fn joined(rendered: &[(String, f32)]) -> String {
        rendered
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\u{1}")
    }

    #[test]
    fn plain_paragraph_renders() {
        let r = rendered("Hello markdown **world**");
        assert!(!r.is_empty(), "no text shapes produced at all");
        let text = joined(&r);
        assert!(text.contains("Hello markdown"), "plain text run missing: {text:?}");
        assert!(text.contains("world"), "bold run missing: {text:?}");
        assert!(
            r.iter().any(|(_, h)| *h > 5.0),
            "all galleys have zero height: {r:?}"
        );
    }

    #[test]
    fn list_code_heading_render() {
        let r = rendered("# Title\n\n- item one\n- **item two**\n\n```rust\nfn main() {}\n```\n");
        let text = joined(&r);
        assert!(text.contains("Title"), "heading missing: {text:?}");
        assert!(text.contains("item one"), "list item missing: {text:?}");
        assert!(text.contains("fn main()"), "code block missing: {text:?}");
    }

    #[test]
    fn dimmed_thinking_renders_markdown() {
        let r = rendered_dimmed("Let me **think** about `foo` and [docs](https://x).", true);
        assert!(!r.is_empty(), "dimmed markdown produced no text shapes");
        let text = joined(&r);
        assert!(text.contains("think"), "bold run missing: {text:?}");
        assert!(text.contains("foo"), "code run missing: {text:?}");
    }
    /// Horizontal bounding box (min_x, max_x) of a shape.
    fn shape_x_bounds(shape: &egui::Shape) -> (f32, f32) {
        match shape {
            egui::Shape::Text(t) => {
                // `galley.rect` is LOCAL to the shape; the on-screen
                // position is `pos + galley.rect`.
                (t.pos.x + t.galley.rect.min.x, t.pos.x + t.galley.rect.max.x)
            }
            egui::Shape::Rect(r) => (r.rect.min.x, r.rect.max.x),
            egui::Shape::Circle(c) => {
                let r = c.radius + c.stroke.width / 2.0;
                (c.center.x - r, c.center.x + r)
            }
            egui::Shape::Path(p) => p
                .points
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                    (lo.min(p.x), hi.max(p.x))
                }),
            egui::Shape::Mesh(m) => m
                .vertices
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), v| {
                    (lo.min(v.pos.x), hi.max(v.pos.x))
                }),
            _ => (f32::MAX, f32::MIN),
        }
    }

    /// Reproduce the chat bubble row (avatar + full-width frame +
    /// `draw_markdown`) in a headless ctx with a fixed-width column, and
    /// return how many pixels the drawn row exceeds the column width.
    /// (The bubble must dock exactly to the column's right edge, never
    /// spill past it — see the `take_available_width` rows in `bubbles.rs`.)
    /// Trace where the available width vanishes in the nested list
    /// structure (debug helper for the overflow investigation).
    #[test]
    fn trace_list_widths() {
        use egui::{Align, Layout, Margin};
        const COL_W: f32 = 560.0;
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::emoji_fonts());
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(COL_W + 200.0, 4000.0),
        ));
        let trace = std::cell::RefCell::new(Vec::<(&'static str, f32)>::new());
        let out = ctx.run_ui(raw, |ui| {
            ui.set_width(COL_W);
            ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                ui.add_sized(egui::vec2(28.0, 28.0), egui::Label::new("A"));
                ui.add_space(8.0);
                ui.scope(|ui| {
                    ui.take_available_width();
                    ui.vertical(|ui| {
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgb(38, 42, 50))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.take_available_width();
                                ui.vertical(|ui| {
                                    trace.borrow_mut().push(("bubble content", ui.available_width()));
                                    // top-level item with a nested sublist
                                    ui.horizontal(|inner| {
                                        inner.label("•");
                                        inner.scope(|s| {
                                            let w = s.available_width();
                                            s.set_width(w.max(20.0));
                                            trace.borrow_mut().push(("item scope", w.max(20.0)));
                                            // nested block
                                            s.vertical(|v| {
                                                trace.borrow_mut().push(("nested vertical", v.available_width()));
                                                v.horizontal(|inner2| {
                                                    inner2.label("•");
                                                    inner2.scope(|s2| {
                                                        let w2 = s2.available_width();
                                                        s2.set_width(w2.max(20.0));
                                                        trace.borrow_mut().push(("nested item scope", w2.max(20.0)));
                                                    });
                                                });
                                            });
                                        });
                                    });
                                });
                            });
                    });
                });
            });
        });
        out.drop_without_applying_deltas();
        for (name, w) in trace.borrow().iter() {
            eprintln!("trace: {name} = {w}");
        }
    }

    /// Regression: no markdown element may push the bubble row wider than
    /// the chat column (the bubble must dock to the column's right edge).
    #[test]
    fn bubble_row_never_exceeds_column_width() {
        let long_word = "x".repeat(400);
        let long_line = "y".repeat(400);
        let samples: &[(&str, &str)] = &[
            (
                "plain",
                "A fairly long plain paragraph that wraps across several lines so the label \
                 uses the full available width of the bubble and nothing should spill out \
                 past the right edge of the chat column when it reflows.",
            ),
            (
                "inline",
                &format!(
                    "Mix of **bold**, *italic*, ~~strike~~, `inline code` and a \
                     [link](https://example.com/{long_word}) plus a long unbroken token \
                     {long_word} to stress the wrap logic."
                ),
            ),
            ("heading", "# Heading one\n\n## Heading two with a rather long title text\n"),
            (
                "bullets",
                "- item one\n- **item two** with more text that keeps going and goes on and on\n  \
                 - nested item\n  - another nested item with a longish label text here\n- three",
            ),
            ("ordered", "1. first step\n2. second step with a longer description text\n3. third"),
            ("tasklist", "- [x] done item\n- [ ] todo item with a longer description here\n"),
            ("code_short", "```rust\nfn main() {}\n```\n"),
            ("code_long", &format!("```\n{long_line}\nfn main() {{}}\n```\n")),
            ("table_2", "| a | b |\n|---|---|\n| 1 | 2 |\n"),
            ("table_3", "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n"),
            ("table_4", "| a | b | c | d |\n|---|---|---|---|\n| 1 | 2 | 3 | 4 |\n"),
            (
                "table_6",
                "| a | b | c | d | e | f |\n|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 |\n",
            ),
            (
                "quote",
                "> quoted **text** that is fairly long so it wraps inside the quote bar\n>\n> second paragraph",
            ),
            ("rule", "before\n\n---\n\nafter\n"),
            (
                "kitchen",
                &format!(
                    "**Status:** done\n\n**Details:**\n- changed *thing* with `code`\n- another \
                     bullet that is intentionally made quite long so it wraps across the full \
                     width of the bubble content area\n\n```rust\nfn main() {{}}\n{long_line}\n```\n\n\
                     | col1 | col2 | col3 |\n|---|---|---|\n| a | b | c |\n",
                ),
            ),
        ];
        let mut offenders = Vec::new();
        for (name, md) in samples {
            let over = bubble_row_overflow_x(md);
            if over > 0.5 {
                offenders.push(format!("{name}={over:.1}px"));
            }
        }
        assert!(
            offenders.is_empty(),
            "bubble row wider than column: {}",
            offenders.join(", ")
        );
    }

    /// Print every visible shape of `text` that extends past the column
    /// (temporary debug for the overflow hunt).
    #[test]
    fn dump_overflowing_shapes() {
        use egui::{Align, Layout, Margin};
        const COL_W: f32 = 560.0;
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::emoji_fonts());
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(COL_W + 200.0, 4000.0),
        ));
        let long_line = "y".repeat(400);
        let text = format!(
            "**Status:** done\n\n**Details:**\n- changed *thing* with `code`\n- another \
             bullet that is intentionally made quite long so it wraps across the full \
             width of the bubble content area\n\n```rust\nfn main() {{}}\n{long_line}\n```\n\n\
             | col1 | col2 | col3 |\n|---|---|---|\n| a | b | c |\n"
        );
        let out = ctx.run_ui(raw, |ui| {
            ui.set_width(COL_W);
            ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                ui.add_sized(egui::vec2(28.0, 28.0), egui::Label::new("A"));
                ui.add_space(8.0);
                ui.scope(|ui| {
                    ui.take_available_width();
                    ui.vertical(|ui| {
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgb(38, 42, 50))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.take_available_width();
                                draw_markdown(ui, &text, 13.5, egui::Color32::WHITE, &Theme::dark());
                            });
                    });
                });
            });
        });
        for cs in &out.shapes {
            let (lo, hi) = shape_x_bounds(&cs.shape);
            {
                let (ty0, ty1) = match &cs.shape {
                    egui::Shape::Text(t) => (
                        t.pos.y + t.galley.rect.min.y,
                        t.pos.y + t.galley.rect.max.y,
                    ),
                    _ => (f32::NAN, f32::NAN),
                };
                let kind = match &cs.shape {
                    egui::Shape::Text(t) => format!("Text {:?}", t.galley.job.text.chars().take(20).collect::<String>()),
                    egui::Shape::Rect(r) => format!("Rect fill={:?}", r.fill),
                    egui::Shape::Circle(_) => "Circle".into(),
                    egui::Shape::Path(p) => format!("Path n={}", p.points.len()),
                    egui::Shape::Mesh(_) => "Mesh".into(),
                    _ => "other".into(),
                };
                eprintln!(
                    "SHAPE {kind} x=[{lo:.1}..{hi:.1}] y=[{ty0:.1}..{ty1:.1}] clipx=[{:.1}..{:.1}]",
                    cs.clip_rect.min.x,
                    cs.clip_rect.max.x
                );
            }
        }
        out.drop_without_applying_deltas();
    }

    fn bubble_row_overflow_x(text: &str) -> f32 {
        use egui::{Align, Layout, Margin};
        const COL_W: f32 = 560.0;
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::emoji_fonts());
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(COL_W + 200.0, 4000.0),
        ));
        let out = ctx.run_ui(raw, |ui| {
            ui.set_width(COL_W);
            ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                // avatar
                ui.add_sized(egui::vec2(28.0, 28.0), egui::Label::new("A"));
                ui.add_space(8.0);
                // content column
                ui.scope(|ui| {
                    ui.take_available_width();
                    ui.vertical(|ui| {
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgb(38, 42, 50))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.take_available_width();
                                draw_markdown(
                                    ui,
                                    text,
                                    13.5,
                                    egui::Color32::from_rgb(235, 238, 242),
                                    &Theme::dark(),
                                );
                            });
                    });
                });
            });
        });
        // Visible part of each shape = shape x-range ∩ clip rect's x-range
        // (content in a horizontal scroll area is clipped, so the unclipped
        // galley rect must not count as overflow).
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        for cs in &out.shapes {
            let (mut lo, mut hi) = shape_x_bounds(&cs.shape);
            lo = lo.max(cs.clip_rect.min.x);
            hi = hi.min(cs.clip_rect.max.x);
            if hi > lo {
                min_x = min_x.min(lo);
                max_x = max_x.max(hi);
            }
        }
        out.drop_without_applying_deltas();
        (max_x - min_x) - COL_W
    }
}
