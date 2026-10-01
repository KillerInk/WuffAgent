//! Markdown rendering for chat messages.
//!
//! Parses GFM-flavoured markdown (pulldown-cmark: tables, strikethrough,
//! task lists) and renders it into the existing chat theme. Used for AI
//! messages in `bubbles.rs`; user messages and thinking blocks stay plain
//! text.
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

/// Font family used for bold runs and headings. Registered in `fonts.rs`
/// (Segoe UI Semibold on Windows, Arial Bold on macOS; when the file is
/// missing it falls back to the regular proportional fonts).
const STRONG_FAMILY: &str = "Strong";

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
    let mut parser = Parser::new_ext(text, options());
    let mut lists = Vec::new();
    let mut queue = VecDeque::new();
    draw_blocks(&mut parser, ui, theme, size, color, &mut lists, &mut queue);
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

/// Render the stream of block-level events until the parser/queue runs out.
/// `queue` holds events the caller already pulled (e.g. an item's first
/// inline event when sniffing for a task-list marker).
fn draw_blocks<'a>(
    parser: &mut Parser<'a>,
    ui: &mut egui::Ui,
    theme: &Theme,
    size: f32,
    color: egui::Color32,
    lists: &mut Vec<ListInfo>,
    queue: &mut VecDeque<Event<'a>>,
) {
    let mut job: Option<egui::epaint::text::LayoutJob> = None;
    let mut style = InlineStyle::default();

    loop {
        let Some(event) = queue.pop_front().or_else(|| parser.next()) else {
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
                    let hsize = heading_font_size(level);
                    let mut hj = new_job();
                    let mut hs = InlineStyle {
                        bold: 1,
                        ..Default::default()
                    };
                    let mut done = false;
                    while !done {
                        match queue.pop_front().or_else(|| parser.next()) {
                            Some(Event::End(TagEnd::Heading(_))) | None => done = true,
                            Some(ev) => apply_inline_event(
                                &mut hj,
                                &ev,
                                &mut hs,
                                hsize,
                                theme.text_primary,
                                theme,
                            ),
                        }
                    }
                    ui.add(egui::Label::new(hj).wrap());
                    ui.add_space(4.0);
                }
                Tag::CodeBlock(kind) => {
                    flush_job(&mut job, ui);
                    let lang = match &kind {
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
                        match queue.pop_front().or_else(|| parser.next()) {
                            Some(Event::Text(t)) => buf.push_str(&t),
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
                            parser,
                            ui,
                            theme,
                            size - 0.5,
                            theme.text_secondary,
                            lists,
                            queue,
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
                Tag::Item => draw_list_item(parser, ui, theme, size, color, lists, queue),
                Tag::Table(_) => {
                    flush_job(&mut job, ui);
                    ui.add_space(4.0);
                    render_table(parser, ui, theme, size, color, queue);
                    ui.add_space(4.0);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush_job(&mut job, ui);
                    ui.add_space(4.0);
                }
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
                apply_inline_event(j, &event, &mut style, size, color, theme);
            }
        }
    }
    flush_job(&mut job, ui);
}

/// One list item: marker (bullet / number / task checkbox) + content.
fn draw_list_item<'a>(
    parser: &mut Parser<'a>,
    ui: &mut egui::Ui,
    theme: &Theme,
    size: f32,
    color: egui::Color32,
    lists: &mut Vec<ListInfo>,
    queue: &mut VecDeque<Event<'a>>,
) {
    ui.add_space(2.0);

    // If the item's first inline event is a TaskListMarker, pull it out so
    // the checkbox can replace the bullet.
    let mut task: Option<bool> = None;
    if let Some(e1) = queue.pop_front().or_else(|| parser.next()) {
        match e1 {
            Event::Start(Tag::Paragraph) => {
                if let Some(e2) = queue.pop_front().or_else(|| parser.next()) {
                    match e2 {
                        Event::TaskListMarker(b) => task = Some(b),
                        other => queue.push_front(other),
                    }
                }
                queue.push_front(Event::Start(Tag::Paragraph));
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
        inner.scope(|ui| {
            ui.set_width(ui.available_width().max(20.0));
            draw_blocks(parser, ui, theme, size, color, lists, queue);
        });
    });
    ui.add_space(2.0);
}

/// Collect a table (header + rows of inline jobs) and draw it with
/// equal-width columns.
fn render_table(
    parser: &mut Parser<'_>,
    ui: &mut egui::Ui,
    theme: &Theme,
    size: f32,
    color: egui::Color32,
    queue: &mut VecDeque<Event<'_>>,
) {
    let mut header: Vec<egui::epaint::text::LayoutJob> = Vec::new();
    let mut rows: Vec<Vec<egui::epaint::text::LayoutJob>> = Vec::new();
    let mut in_head = false;
    let mut row: Option<Vec<egui::epaint::text::LayoutJob>> = None;
    let mut cell: Option<egui::epaint::text::LayoutJob> = None;
    let mut cell_style = InlineStyle::default();

    loop {
        let Some(ev) = queue.pop_front().or_else(|| parser.next()) else {
            break;
        };
        match ev {
            Event::Start(Tag::TableHead) => in_head = true,
            Event::End(TagEnd::TableHead) => in_head = false,
            Event::Start(Tag::TableRow) => row = Some(Vec::new()),
            Event::Start(Tag::TableCell) => {
                cell = Some(new_job());
                cell_style = if in_head {
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
                    apply_inline_event(c, &ev, &mut cell_style, size, color, theme);
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
    let col_w = ((ui.available_width() - 8.0) / n as f32).max(48.0);

    let draw_row = |ui: &mut egui::Ui, cells: &[egui::epaint::text::LayoutJob]| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for c in cells {
                let mut cc = c.clone();
                cc.wrap.break_anywhere = true;
                ui.scope(|ui| {
                    ui.set_width(col_w);
                    ui.add(egui::Label::new(cc).wrap());
                });
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
) {
    match event {
        Event::Text(t) => append_inline(job, t, *style, size, color, theme, false),
        Event::Code(c) => append_inline(job, c, *style, size, color, theme, true),
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
fn append_inline(
    job: &mut egui::epaint::text::LayoutJob,
    text: &str,
    style: InlineStyle,
    size: f32,
    color: egui::Color32,
    theme: &Theme,
    code: bool,
) {
    let (font_id, color, background, expand_bg) = if code {
        (
            egui::FontId::monospace(size - 0.5),
            theme.code_text,
            theme.code_bg,
            1.0,
        )
    } else {
        let font_id = if style.bold > 0 {
            egui::FontId::new(size, egui::FontFamily::Name(STRONG_FAMILY.into()))
        } else {
            egui::FontId::proportional(size)
        };
        let color = if style.link > 0 { theme.accent } else { color };
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
